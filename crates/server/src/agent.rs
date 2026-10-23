//! The agent loop: one user message in, a stream of events out.
//!
//! A *step* is a single model request plus the tools it called. A *turn* runs
//! steps until the model stops asking for tools or the user cancels; there is no
//! step ceiling. After every step the transcript is already durable, so a crash
//! mid-turn leaves a replayable session rather than a hole.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::FutureExt;
use protocol::{Event, LogLevel, ResponseEvent, SessionStatus, ToolAnswerEvent, ToolStream};
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

/// Share of a model's context window that compaction fills before folding the
/// transcript. The rest is headroom for what a step adds after the check: the
/// request's own output, and the tool results it asks for.
const DEFAULT_COMPACT_PERCENT: u8 = 70;

pub struct Agent {
    pub store: Store,
    /// What a session talks to unless it named its own `--llm`.
    pub llm: Arc<LlmClient>,
    /// The machine's roster, which is what `--llm` names are resolved against.
    pub providers: &'static Catalog,
    /// What a session's own entry inherits: timeouts, retries, instructions.
    pub llm_config: LlmConfig,
    /// Clients built for sessions that named their own entry, by `provider/model`.
    ///
    /// A client builds a connection pool, so a turn that used one per step would
    /// pay for it on every request. Keyed by spec rather than by session
    /// because sessions that choose the same model should share it.
    pub clients: std::sync::Mutex<std::collections::HashMap<String, Arc<LlmClient>>>,
    /// Context window of [`Self::llm`]'s model, for a session that asks for a
    /// percentage without naming a model of its own.
    pub default_context: Option<u64>,
    pub tools: Arc<ToolRegistry>,
    pub bus: EventBus,
    /// Compress once a request would carry more than this many input tokens.
    /// 0 disables it.
    pub compress_above_tokens: u64,
    /// Messages a compression leaves alone at the end of the transcript.
    pub keep_recent: usize,
    /// Where capped tool output is spilled. Comes from the config: an agent
    /// that cannot write it anywhere can still cap, it just loses the tail.
    pub output_dir: PathBuf,
    /// Detached jobs of every session. Reaches the tools through `ToolContext`,
    /// which is the only place a tool call gets to start or stop one.
    pub detached: DetachedRegistry,
}

impl Agent {
    /// Runs one turn, publishing events as it goes. Never returns an error:
    /// failures are reported on the bus and recorded in the session's status.
    ///
    /// `task` is the pending row this turn was started from. Its message is
    /// already in the transcript, so the step-boundary inbox must not hand it
    /// back -- that would append the operator's line twice.
    pub async fn run_turn(&self, session: &SessionHandle, user_text: &str, task: Option<i64>) {
        self.turn(session, Some(user_text), task).await;
    }

    /// Replays a turn that a shutdown interrupted.
    ///
    /// The user message is already in the transcript: it is written before the
    /// turn starts, which is the only reason the turn could be recorded as
    /// pending at all. Appending it again made every restart show the operator's
    /// line twice, and added one more copy to the history per resume.
    pub async fn resume_turn(&self, session: &SessionHandle, task: Option<i64>) {
        self.turn(session, None, task).await;
    }

    async fn turn(&self, session: &SessionHandle, user_text: Option<&str>, task: Option<i64>) {
        // A panic anywhere in the loop used to be the end of the turn with
        // nothing left to clean up after it: the session kept the "running"
        // status it was given on entry, no request was in flight, nothing was
        // logged, and nothing ever moved again -- which is indistinguishable
        // from a provider that has stopped answering. Catching it here turns
        // that silence into an error on screen and leaves the session in a
        // state the operator can do something about.
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
        // Queue behind whatever is already working on this session. Taking the
        // gate *before* `new_turn` is the whole point: `new_turn` cancels the
        // running turn, so a second turn that took it first would kill the turn
        // it is meant to be waiting behind. Starting a turn used to mean
        // "interrupt"; now it means "get in line".
        let epoch = session.cancel_epoch();
        let queued = session.busy();
        let _gate = session.claim().await;
        if queued {
            // Otherwise a queued turn looks like the server froze: nothing
            // moves until the turn in front of it is done, which for a long
            // turn is minutes.
            tracing::info!(session = %name, "turn waited for the session to be free");
        }
        if session.cancel_epoch() != epoch {
            // Cancelled while queued. Running now would look like the
            // `interrupt_session` did nothing, and the message it was going to
            // deliver is dropped just like any other interrupted turn: whoever
            // spawned this clears the pending row unless a shutdown asked to
            // keep it.
            tracing::info!(session = %name, "turn dropped: cancelled while queued");
            return;
        }
        if session.is_deleted() {
            // The wait was behind a `del_session`. There is nothing to write
            // to any more, and writing would resurrect rows the operator asked
            // to be gone.
            tracing::info!(session = %name, "turn dropped: session was deleted while queued");
            return;
        }
        let (turn, cancel) = session.new_turn();
        let started = std::time::Instant::now();
        // Which model this turn talks to, and how much transcript it may carry
        // before compaction. Resolved once, at the top: a session can be pointed
        // at a different provider between turns, but every step of one turn has
        // to agree on where it is sending.
        let (llm, budget) = match self.route(&name, &session.tuning()) {
            Ok(route) => route,
            Err(e) => {
                self.fail(&name, id, "resolving the session's llm", e);
                return;
            }
        };
        // Read once, like the route: a `set_session --thinking-effort` between
        // turns applies to the next turn, and every step of this one sends the
        // level it started with.
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

        // No step budget. A turn runs until the model stops asking for tools,
        // the user cancels, or a request fails.
        //
        // There is deliberately no ceiling: a turn that is meant to work for
        // days has no number of steps that is right in advance. Any number is
        // either too small -- it stops mid-task and waits for a human to type
        // "continue", which is not autonomy, it is a babysitter -- or large
        // enough that it is not a limit at all. What actually keeps a long run
        // finite is the work itself ending, and what keeps it from spinning is
        // a request that fails, both of which are reported instead of silently
        // truncating.
        // Every exit from this loop carries its own outcome, so the value the
        // match below sees is always set on the way out; there is no default
        // and no fall-through.
        let mut step: u32 = 0;

        let outcome = loop {
            if cancel.is_cancelled() {
                break TurnOutcome::Cancelled;
            }
            // Step boundary: everything that arrived while the previous step's
            // tools were running becomes part of the transcript here, so the
            // request built by `step` below carries it. Injecting earlier would
            // put a user line between a tool call and its result, which
            // providers reject outright.
            self.deliver_inbox(id, &name, task);
            // Before building the request: if the transcript has grown past the
            // budget, fold its older half into a summary. Doing it here rather
            // than after a step means the very next request is the small one.
            self.maybe_compress(id, &name, &llm, budget, &cancel).await;
            tracing::info!(session = %name, step, "step start");
            let attempt = Attempt {
                number: step,
                llm: &llm,
                effort: effort.as_deref(),
            };
            match self.step(id, &name, turn, &cancel, &attempt).await {
                Ok(StepResult::Finished) => {
                    // The model says it is done, but something may have landed
                    // while it was finishing. Deliver it and go round again
                    // rather than end the turn with a message unread because it
                    // arrived one step too late.
                    if self.deliver_inbox(id, &name, task) > 0 {
                        step = step.wrapping_add(1);
                        continue;
                    }
                    break TurnOutcome::Done;
                }
                Ok(StepResult::NeedsTools(calls)) => {
                    if self.run_tools(session, id, &name, &cancel, calls).await {
                        break TurnOutcome::Cancelled;
                    }
                }
                Ok(StepResult::Stopped) => break TurnOutcome::Cancelled,
                Err(e) => {
                    self.error(&name, "model request", e);
                    break TurnOutcome::Failed;
                }
            }
            // Wrapping rather than saturating: the counter is only a label in
            // logs, and a turn long enough to reach the end of u32 has bigger
            // things to worry about than the number restarting.
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

    /// Where this turn sends, and how many input tokens it may carry before
    /// compacting.
    ///
    /// Both come from the session's tuning, falling back to the server's own
    /// configuration. A percentage only has a size once a context window is
    /// known, which is why the window travels with the model.
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

    /// The compaction threshold in input tokens.
    ///
    /// Zero means "off", everywhere and without exception, so a server that
    /// wants no compaction can say so and be obeyed rather than being second
    /// guessed by a default percentage.
    fn budget(&self, name: &str, context: Option<u64>, percent: Option<u8>) -> u64 {
        // Without a context window a percentage has no size. Saying so beats
        // quietly compacting at a different point than the one that was asked
        // for.
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
        // Built outside the lock: `LlmClient::new` is the expensive part and two
        // sessions racing to build the same one is cheaper than serialising
        // every turn behind a connection pool.
        let client = Arc::new(LlmClient::new(&self.llm_config.for_entry(entry, api_key))?);
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(spec, client.clone());
        Ok(client)
    }

    /// Reports a failure that stops the turn before it starts, and leaves the
    /// session in a state the operator can act on rather than "running".
    fn fail(&self, name: &str, id: i64, what: &str, e: anyhow::Error) {
        self.error(name, what, e);
        let _ = self.store.set_status(id, SessionStatus::Error);
        self.bus.publish(Event::Status {
            session: name.to_string(),
            status: SessionStatus::Error,
            detail: Some("see log".to_string()),
        });
    }

    /// Moves everything the session's inbox collected into the transcript and
    /// returns how many messages it delivered.
    ///
    /// This is the *only* place a mid-turn message enters a running turn, and
    /// it is called at step boundaries on purpose. A message that arrived while
    /// tools were running cannot be written the moment it arrives: a user line
    /// between a `function_call` and its `function_call_output` is a request
    /// the provider rejects. At a step boundary the previous step is complete,
    /// so the user line is a legal trailing item and the next request carries it.
    ///
    /// The row is deleted in the same breath as the transcript write (see
    /// `take_pending_tasks`), so a message is delivered exactly once even if the
    /// process dies in between.
    fn deliver_inbox(&self, id: i64, name: &str, own: Option<i64>) -> usize {
        let Ok(tasks) = self.store.take_pending_tasks(name, own) else {
            return 0;
        };
        let mut delivered = 0;
        for task in tasks {
            let Some(text) = task.payload.get("message").and_then(|v| v.as_str()) else {
                // A row with no message cannot be delivered; leaving it would
                // block the queue forever, and its kind is not one this loop
                // knows how to replay.
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

    /// Folds the older part of the transcript into one summary when the next
    /// request would otherwise carry more than the budget allows.
    ///
    /// The rows are not deleted: `summarised_upto` only marks where the summary
    /// takes over, so the full transcript stays readable while every later
    /// request starts from the summary plus recent messages. Without this a
    /// long turn grows without bound until the provider refuses it outright.
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
        // How full the context is, as the provider last measured it. Not an
        // estimate and not derived from text length: `usage.input_tokens`
        // already covers the instructions, the tool schemas, the summary and
        // the whole transcript, tokenised by the party that will bill for it.
        // The only thing it misses is what this step adds after the check --
        // one response plus its tool output -- which is what the budget's
        // headroom is for.
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
        // Only what a request would actually carry is eligible: anything
        // already behind the last summary is not sent again.
        let live: Vec<&(i64, String)> = index.iter().filter(|(s, _)| *s > upto).collect();
        let Some(cut) = choose_cut(&live, self.keep_recent) else {
            return;
        };
        // Asked for by sequence rather than cut out of the whole transcript:
        // the mapping from rows to items is not one-to-one, so counting rows
        // covered a range that was not the one the cut named -- and a summary
        // that swallows a message from after the cut loses it.
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
                // The last measured size is now history: it described a request
                // built from the prefix that was just folded away. Clearing it
                // means "unknown until the next request reports", which stops
                // the following step from compacting again on a stale number.
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
                        // Recorded, never computed. `input_tokens` is the
                        // provider's own count of what this request carried --
                        // instructions, tool schemas, summary and transcript --
                        // measured by the tokeniser that will also measure the
                        // next request. No arithmetic over text length lands on
                        // that number for every provider we speak to.
                        if let Some(input) = u.input_tokens {
                            let _ = self.store.set_context_tokens(id, input);
                        }
                    }
                }
                LlmEvent::Failed { message } => {
                    failure = Some(message);
                    break;
                }
                // A retry is the only news a stalled request ever produces, so
                // it goes to the operator instead of into the transcript: the
                // turn looks alive, and the reason it is slow is on screen.
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
            let _ = self.store.append_message(
                id,
                "tool_result",
                &json!({"call_id": call.call_id, "output": outcome.output}),
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
///
/// A bundle rather than three more parameters, because the three travel
/// together and nothing else in the turn needs them: `step` already takes the
/// session, the turn, the cancellation token.
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
///
/// `configured` is the server's own number, used only when the roster does not
/// say how big the model's window is. A known window is the better measure: it
/// makes the threshold follow the model instead of the other way round.
fn compaction_budget(configured: u64, context: Option<u64>, percent: Option<u8>) -> u64 {
    if configured == 0 {
        return 0;
    }
    let Some(window) = context.filter(|w| *w > 0) else {
        return configured;
    };
    providers::tokens_for_percent(window, percent.unwrap_or(DEFAULT_COMPACT_PERCENT))
}

/// Where a summary may take over: everything up to and including the returned
/// seq is compressed, everything after it is kept verbatim.
///
/// The constraint is the API's, not ours: a `function_call_output` with no
/// `function_call` in front of it is rejected, and a reasoning item whose
/// predecessor vanished is at best wasted. So the boundary is placed where the
/// next message can stand on its own -- a user turn or a tool call.
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
            HistoryItem::ToolResult { output, .. } => {
                p.push_str(&format!("=> {}\n", clip(output, 800)))
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
/// Counted and cut in characters, not bytes. The transcript is full of
/// Chinese, and slicing a `&str` at a byte offset is a panic when that
/// offset lands inside a character. It used to be, and the panic took the
/// whole turn down where nothing was left to move it: the session sat at
/// "running" with no request in flight and no error anywhere, which reads
/// exactly like a provider that has stopped answering.
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

    /// Cutting a summary prompt used to slice by bytes, which panics the
    /// moment the cut lands inside a Chinese character -- taking the turn
    /// down mid-flight with no request in flight and nothing logged.
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

    /// The API rejects a `function_call_output` without its `function_call`, so
    /// the cut must never leave one of those at the front of what survives.
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
        // Cutting at 4 would leave 5 (a tool_result) first, and at 5 leaves the
        // text on its own -- that is as far back as it can safely go.
        assert_eq!(choose_cut(&borrow, 2), Some(5));
    }

    /// When nothing in the recent window can start a request on its own, leave
    /// the transcript alone rather than send something the API would reject.
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

    /// A known window decides the threshold, so a model with a bigger window
    /// gets a bigger budget without anyone retuning the config.
    #[test]
    fn a_known_window_beats_the_configured_number() {
        assert_eq!(compaction_budget(40_000, Some(1_000_000), None), 700_000);
        assert_eq!(compaction_budget(40_000, Some(200_000), Some(50)), 100_000);
        // No window in the roster: the configured number is all there is.
        assert_eq!(compaction_budget(40_000, None, Some(50)), 40_000);
        assert_eq!(compaction_budget(40_000, None, None), 40_000);
    }

    #[test]
    fn long_output_is_clipped_in_the_summary_prompt() {
        let long = "x".repeat(5_000);
        let clipped = clip(&long, 800);
        assert!(clipped.len() < 1_000);
        assert!(clipped.contains("more characters"));
    }
}
