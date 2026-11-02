//! The agent loop: one user message in, a stream of events out.
//! See `docs/notes/server-agent.md`.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::FutureExt;
use protocol::{
    Event, LogLevel, ResponseEvent, ResponsePart, SessionStatus, ToolAnswerEvent, ToolStream,
};
use serde_json::json;
use tokio::sync::mpsc;

use crate::bus::EventBus;
use crate::config::ModelConfig;
use crate::llm::{LlmClient, LlmEvent};
use crate::providers::{self};
use crate::session::SessionHandle;
use crate::store::Store;
use crate::tools::detached::DetachedRegistry;
use crate::tools::{ToolContext, ToolRegistry};

/// Share of the window the notice is sent at; the rest is headroom for one step's output.
pub const DEFAULT_CONTEXT_LIMIT_PERCENT: u8 = 70;

pub struct Agent {
    pub store: Store,
    /// The machine's roster, which `--model` names resolve against. A live view:
    /// a reload reaches every later route.
    pub providers: providers::Handle,
    /// What a session's own entry inherits: timeouts and retries.
    pub model_config: ModelConfig,
    /// Clients for sessions that named their own model, by `provider/model_name`.
    ///
    /// # Invariant
    ///
    /// Keyed by the roster read that produced each one: a cached client carries the
    /// base URL, the key and the model it was built from, so one kept across a reload
    /// sends the new model to the old endpoint.
    pub clients: std::sync::Mutex<std::collections::HashMap<String, (u64, Arc<LlmClient>)>>,
    /// [`Self::llm`]'s context window, for a percentage set without naming a model.
    pub default_context: Option<u64>,
    pub tools: Arc<ToolRegistry>,
    pub bus: EventBus,
    /// How often a cached prefix is assumed to be gone: the plan's hyperparameter.
    pub cache_invalid_rate: f64,
    /// How long a cached prefix survives, in millis; `0` never expires.
    pub cache_ttl_ms: u64,
    /// What the session's model charges, from the roster.
    pub price: Option<crate::providers::Price>,
    /// Compress once a request would carry more than this many input tokens.
    /// 0 disables it.
    pub compress_above_tokens: u64,
    /// Where capped tool output is spilled; without it the tail is lost.
    pub output_dir: PathBuf,
    /// Detached jobs of every session, reached through `ToolContext`.
    pub detached: DetachedRegistry,
}

impl Agent {
    /// Writes one row to the log and announces it, in that order.
    ///
    /// # Invariant
    ///
    /// The log is the source of truth and the bus is its notification: the row is
    /// on disk before anybody is told it exists, so a subscriber that reacts to
    /// `Record` can always read what it was told about, and a process that died
    /// between the two loses a notification rather than a fact.
    ///
    /// Every row the agent writes goes through here. Before this, seven call
    /// sites appended to the table and published nothing -- so a `wait --kind
    /// turn_end` had no event to wait for, and the log was a place only a poller
    /// could find out about. A row nobody can subscribe to is a row half-written.
    ///
    /// A failed write is a warning, not a panic: the turn is what matters, and a
    /// log that refuses a row must not take the conversation with it.
    pub fn log_event(&self, session: Option<i64>, kind: &str, payload: &serde_json::Value) {
        let row = match self.store.append_event_row(session, kind, payload) {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(kind, error = %e, "could not write an event row");
                return;
            }
        };
        self.bus.publish(Event::Record {
            id: row.id,
            seq: row.seq,
            session: row
                .session_id
                .and_then(|id| self.store.name_of_session(id).ok().flatten()),
            kind: row.kind,
            payload: payload.clone(),
            at: row.created_at,
        });
    }

    /// Runs one already-claimed turn whose user messages and obligations are durable.
    pub async fn run_turn(
        &self,
        session: &SessionHandle,
        tasks: Vec<i64>,
        epoch: u64,
        gate: tokio::sync::OwnedMutexGuard<()>,
    ) {
        let started = std::time::Instant::now();
        let outcome = AssertUnwindSafe(self.turn_inner(session, epoch, gate))
            .catch_unwind()
            .await;
        let (outcome, steps) = match outcome {
            Ok(pair) => pair,
            Err(payload) => {
                let message = panic_message(&payload);
                tracing::error!(session = %session.name(), "turn panicked: {message}");
                self.bus.publish(Event::Log {
                    level: LogLevel::Error,
                    message: format!("{}: turn panicked: {message}", session.name()),
                });
                (TurnOutcome::Failed, 0)
            }
        };
        // The turn's outcome as a row, not only as a line: a reader that has the
        // database should not have to match prose to learn why a run stopped.
        self.log_event(
            Some(session.id()),
            "turn_end",
            &serde_json::json!({
                "outcome": outcome.label(),
                "steps": steps,
                "elapsed_ms": started.elapsed().as_millis() as u64,
            }),
        );
        let (status, detail) = match outcome {
            TurnOutcome::Done => (SessionStatus::Idle, None),
            TurnOutcome::Cancelled => (SessionStatus::Idle, Some("cancelled".to_string())),
            TurnOutcome::Failed => (SessionStatus::Error, Some("see log".to_string())),
        };
        let preserve = session.take_resumable();
        if let Err(e) =
            self.store
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
    ) -> (TurnOutcome, u32) {
        let name = session.name().to_string();
        let id = session.id();
        if session.cancel_epoch() != epoch {
            // Cancelled while queued: running looks like the interrupt did nothing.
            tracing::info!(session = %name, "turn dropped: cancelled while queued");
            return (TurnOutcome::Cancelled, 0);
        }
        if session.is_deleted() {
            tracing::info!(session = %name, "turn dropped: session was deleted before starting");
            return (TurnOutcome::Cancelled, 0);
        }
        let (turn, cancel) = session.new_turn();
        let started = std::time::Instant::now();

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
                break (TurnOutcome::Cancelled, step);
            }
            // Step boundary: a user line between a call and its result is rejected, so inject here.
            self.deliver_inbox(id, &name);
            // Everything a request is built from is read here, every step: a
            // `set_session` that lands while this turn runs is meant for the next
            // request, and one step is the longest anything may lag behind it.
            let (llm, effort, budget) = match self.settings(&name, session) {
                Ok(settings) => settings,
                Err(e) => {
                    self.error(id, &name, "resolving the session's provider", e);
                    break (TurnOutcome::Failed, step);
                }
            };
            if step == 0 {
                tracing::info!(
                    session = %name,
                    model = %llm.model(),
                    effort = effort.as_deref().unwrap_or("-"),
                    budget,
                    "turn start"
                );
            }
            // Ask before the request is built, so this is the step that hears it.
            self.maybe_notify_context(session, id, &name, budget);
            let cwd = session.cwd();
            tracing::info!(
                session = %name,
                step,
                model = %llm.model(),
                effort = effort.as_deref().unwrap_or("-"),
                "step start"
            );
            let attempt = Attempt {
                number: step,
                llm: &llm,
                effort: effort.as_deref(),
                cwd: &cwd,
            };
            match self.step(session, id, &name, turn, &cancel, &attempt).await {
                Ok(StepResult::Finished) => {
                    // Something may have landed while it finished; deliver it and go round again.
                    if self.deliver_inbox(id, &name) > 0 {
                        step = step.wrapping_add(1);
                        continue;
                    }
                    break (TurnOutcome::Done, step);
                }
                Ok(StepResult::RanTools) => {}
                Ok(StepResult::Stopped) => break (TurnOutcome::Cancelled, step),
                Err(e) => {
                    self.error(id, &name, "model request", e);
                    break (TurnOutcome::Failed, step);
                }
            }
            // Wrapping, not saturating: the counter is only a log label.
            step = step.wrapping_add(1);
        };

        tracing::info!(session = %name, outcome = ?outcome.0, elapsed = ?started.elapsed(), "turn end");
        outcome
    }

    /// Where the next request sends: the model, the level and the notice budget, all
    /// read from the session as it is now.
    fn settings(
        &self,
        name: &str,
        session: &SessionHandle,
    ) -> anyhow::Result<(Arc<LlmClient>, Option<String>, u64)> {
        let tuning = session.tuning();
        let (llm, budget) = self.route(name, &tuning)?;
        Ok((llm, tuning.thinking_effort, budget))
    }

    /// Where this turn sends, and how many input tokens it may carry before it is worth a notice.
    ///
    /// # Invariant
    ///
    /// Cached by roster entry, so the notice a message's references get and the
    /// turn that reads them are one client.
    pub(crate) fn route(
        &self,
        name: &str,
        tuning: &crate::session::Tuning,
    ) -> anyhow::Result<(Arc<LlmClient>, u64)> {
        let (llm, context) = match &tuning.model {
            // The config's `[model]` is the server's own, never a session's.
            None => anyhow::bail!("{name} states no model: set_session --model"),
            Some(model) => {
                let choice = self
                    .providers
                    .resolve(model)
                    .or_else(|e| self.providers.resolve_by_name(model).ok_or(e))
                    .map_err(|e| anyhow::anyhow!("--model {model}: {e}"))?;
                let client = self.client_for(&choice.entry, choice.api_key)?;
                (client, choice.entry.context)
            }
        };
        let budget = self.budget(name, context, tuning.context_limit);
        Ok((llm, budget))
    }

    /// The notice threshold in input tokens; zero means off without exception.
    fn budget(&self, name: &str, context: Option<u64>, percent: Option<u8>) -> u64 {
        // A percentage has no size without a window; warn rather than guess.
        if self.compress_above_tokens != 0 && context.is_none() && percent.is_some() {
            tracing::warn!(
                session = %name,
                "no context window is known for this model; using the configured token budget"
            );
        }
        notice_budget(self.compress_above_tokens, context, percent)
    }

    /// The client for one roster entry, built once and shared.
    fn client_for(
        &self,
        entry: &providers::Entry,
        api_key: String,
    ) -> anyhow::Result<Arc<LlmClient>> {
        let model = entry.model.clone();
        let generation = self.providers.generation();
        let cached = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&model)
            .cloned();
        if let Some((pinned, client)) = cached {
            if pinned == generation {
                return Ok(client);
            }
        }
        // Built outside the lock: a duplicate client is cheaper than serialising turns.
        let mut cfg = self.model_config.for_entry(entry, api_key);
        // Every client measures its own provider, into this home's store.
        cfg.measure = Some(self.store.clone());
        cfg.price = entry.price;
        let client = Arc::new(LlmClient::new(&cfg)?);
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(model, (generation, client.clone()));
        Ok(client)
    }

    /// Moves the session's inbox into the transcript in one durable transaction.
    fn deliver_inbox(&self, id: i64, name: &str) -> usize {
        match self.store.deliver_pending(id, name) {
            // `waited` is the number an operator reads as "no answer yet": a message
            // written while this turn was mid-step lands only here.
            Ok((delivered, waited_ms)) => {
                if delivered > 0 {
                    tracing::info!(
                        session = %name,
                        delivered,
                        waited = ?std::time::Duration::from_millis(waited_ms.max(0) as u64),
                        "delivered queued messages at a step boundary"
                    );
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
        let Ok((tokens, generation)) = self.store.context_measurement(id) else {
            return;
        };
        if tokens < budget {
            return;
        }
        // Once per crossing: nagging every step teaches nothing, and a context
        // that was cut back is a new crossing.
        if !session.note_context_asked(tokens, budget) {
            return;
        }
        let message = context_notice(crate::prompts::shared(), name, tokens, budget);
        if let Err(e) = self.store.add_pending_task(
            name,
            "inject",
            &json!({
                "session": name,
                "message": message,
                "context_notice": {"tokens": tokens, "limit": budget, "generation": generation},
            }),
        ) {
            self.error(id, name, "queueing the context notice", e);
            return;
        }
        self.log_event(
            Some(id),
            "context_notice",
            &json!({"tokens": tokens, "limit": budget}),
        );
        self.bus.publish(Event::Log {
            level: LogLevel::Info,
            message: format!(
                "{name}: context at {tokens} of {budget} tokens; asked the agent to fold"
            ),
        });
    }

    /// The prose this session sends, read now and stored when it differs.
    ///
    /// # Invariant
    ///
    /// What a request carries is immutable once sent: a row is appended, never
    /// rewritten, and the row's own facts (the tail) are stamped with the first row
    /// and replayed from the store after that. So the prose can follow an edited
    /// template, and the timestamp cannot follow the clock.
    /// See `docs/notes/server-agent.md`.
    fn prompt_in_force(
        &self,
        id: i64,
        name: &str,
        model: &str,
        cwd: &std::path::Path,
        note: Option<&str>,
    ) -> anyhow::Result<crate::store::Prompt> {
        let stored = self.store.prompt(id)?;
        let prompts = crate::prompts::shared();
        // Read, not rendered, and the one text every turn of every session shares.
        let instructions = prompts.system();
        // Stamped with the first row and replayed verbatim after, including when there
        // is none: a row written before the card existed stays without one, because
        // inserting a message now would rewrite what was already sent.
        let tail = match &stored {
            Some(kept) => kept.tail.clone(),
            None => Some(prompts.render(
                "dynamic_tail",
                &[
                    ("session", name),
                    ("build", protocol::BUILD),
                    ("now", &crate::clock::now()),
                    ("cwd", &Self::stated(cwd)),
                ],
            )),
        };
        // Where this session belongs: the row's own answer once there is one, so a
        // later `cd` in its shell is not a rewrite of what was already sent. It is
        // both what the prompt records and where its rules file is read.
        let home = stored
            .as_ref()
            .map(|kept| std::path::PathBuf::from(&kept.cwd))
            .unwrap_or_else(|| std::path::PathBuf::from(Self::stated(cwd)));
        // Built from what the references say now, so an edited body reaches the model.
        let (index, rooted) = match self.index_of(id, name, model, cwd, &home) {
            Some((index, rooted)) => (Some(index), rooted),
            None => match &stored {
                Some(kept) => (kept.index.clone(), kept.rooted.clone()),
                None => (None, Vec::new()),
            },
        };
        let handover = note.map(|s| prompts.render("handover_preamble", &[("handover", s)]));
        let prompt = crate::store::Prompt {
            instructions,
            index,
            tail,
            rooted,
            handover,
            // Where the request said the shell stood, which is the stored row's
            // answer once there is one: a later move is not a rewrite.
            cwd: stored
                .as_ref()
                .map(|kept| kept.cwd.clone())
                .unwrap_or_else(|| Self::stated(&home)),
            stamp: crate::store::stamp(note),
        };
        // Only the texts that go on the wire are compared; `cwd` is a record.
        if let Some(kept) = stored.clone().filter(|kept| !kept.rewritten_by(&prompt)) {
            return Ok(kept);
        }
        // Rewritten prose is a real event: the prefix the provider cached is gone
        // from here, and the ledger will show the miss.
        if stored.is_some() {
            tracing::info!(session = %name, "the prompt in force changed: a new row is sent");
        }
        self.store.append_prompt(id, &prompt)?;
        Ok(prompt)
    }

    /// The one spelling of `cwd` the prompt says.
    ///
    /// # Invariant
    ///
    /// Two spellings of one directory render one string: a re-spelling would rewrite
    /// the cached tail.
    fn stated(cwd: &std::path::Path) -> String {
        let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let text = resolved.display().to_string();
        match text.strip_prefix(r"\\?\") {
            Some(rest) => rest.to_string(),
            None => text,
        }
    }

    /// How many tokens `text` is worth on the roster model named `model`.
    fn tokens_on(&self, model: &str, text: &str) -> usize {
        let ratio = self.store.token_ratio(model);
        (text.len() as f64 * ratio).round().max(1.0) as usize
    }

    /// The index this request carries: the text every reference its messages name
    /// says now, in the order the cache pays least for.
    ///
    /// # Invariant
    ///
    /// The order is `prefix_cache`'s, which is a permutation of what it was given: no
    /// entry is invented for a reference that could not be read, and the spellings
    /// returned are exactly the ones the index carries, so `meta::expand` leaves
    /// those mentions literal.
    /// The rules file is read in `home` and a reference a message names in `cwd`, so
    /// the map does not move when the shell does and a message's own paths still
    /// mean what they said. See `docs/notes/server-agent.md`.
    fn index_of(
        &self,
        id: i64,
        name: &str,
        model: &str,
        cwd: &std::path::Path,
        home: &std::path::Path,
    ) -> Option<(String, Vec<String>)> {
        // The queue as well as the transcript: a message delivered before this row
        // was left literal on the promise that the row would carry it.
        let mut texts = self.store.user_texts(id).ok()?;
        texts.extend(self.store.queued_texts(name).unwrap_or_default());
        let borrowed: Vec<&str> = texts.iter().map(String::as_str).collect();
        let roots = crate::meta::roots(home, cwd, &borrowed);
        if roots.is_empty() {
            return None;
        }
        let rates = crate::prefix_cache::Rates {
            price: self.price,
            invalid: self.cache_invalid_rate,
            ttl_ms: self.cache_ttl_ms,
        };
        let wanted: Vec<(&str, usize, u64)> = roots
            .iter()
            .map(|root| {
                let digest = crate::prefix_cache::digest(&root.content);
                (
                    root.spelling.as_str(),
                    self.tokens_on(&model, &root.content),
                    digest,
                )
            })
            .collect();
        let order = crate::prefix_cache::plan(&self.store, name, "prompt index", &wanted, rates);
        let mut entries: Vec<&crate::meta::Root> = Vec::with_capacity(order.len());
        for spelling in &order {
            if let Some(root) = roots.iter().find(|root| &root.spelling == spelling) {
                entries.push(root);
            }
        }
        let body = entries
            .iter()
            .map(|root| root.text())
            .collect::<Vec<_>>()
            .join("\n\n");
        let index = crate::prompts::shared().render("reference_index", &[("entries", &body)]);
        let rooted: Vec<String> = entries.iter().map(|r| r.spelling.clone()).collect();
        Some((index, rooted))
    }

    /// Records the whole step around the narrower request and tool measurements.
    async fn step(
        &self,
        session: &SessionHandle,
        id: i64,
        name: &str,
        turn: u64,
        cancel: &tokio_util::sync::CancellationToken,
        attempt: &Attempt<'_>,
    ) -> anyhow::Result<StepResult> {
        let started_at = crate::store::now_millis();
        let started = std::time::Instant::now();
        let mut phases = StepPhases::default();
        let result = self
            .step_inner(session, id, name, turn, cancel, attempt, &mut phases)
            .await;
        let outcome = match &result {
            Ok(StepResult::Finished) => "finished",
            Ok(StepResult::RanTools) => "ran_tools",
            Ok(StepResult::Stopped) => "stopped",
            Err(_) => "failed",
        };
        let llm_ms = phases
            .llm_started
            .map(|started| started.elapsed().as_millis() as u64);
        self.log_event(
            Some(id),
            "step_timing",
            &json!({
                "step": attempt.number,
                "turn": turn,
                "started_at": started_at,
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "prompt_ms": phases.prompt_ms,
                "source_ms": phases.source_ms,
                "llm_ms": llm_ms,
                "outcome": outcome,
            }),
        );
        result
    }

    /// One model request. Runs each tool call the moment it arrives.
    async fn step_inner(
        &self,
        session: &SessionHandle,
        id: i64,
        name: &str,
        turn: u64,
        cancel: &tokio_util::sync::CancellationToken,
        attempt: &Attempt<'_>,
        phases: &mut StepPhases,
    ) -> anyhow::Result<StepResult> {
        let Attempt {
            number: step,
            llm,
            effort,
            cwd,
        } = *attempt;
        let note = self.store.handover(id)?;
        let generation = self.store.context_generation(id)?;
        let phase_started = std::time::Instant::now();
        let definitions = self.tools.definitions();
        let prompt = self.prompt_in_force(id, &name, llm.model(), cwd, note.as_deref())?;
        phases.prompt_ms = phase_started.elapsed().as_millis() as u64;
        // Pinned here, so every retry of this request replays the window that was
        // current when it started.
        let phase_started = std::time::Instant::now();
        let source = self.store.request_source(id)?;
        phases.source_ms = phase_started.elapsed().as_millis() as u64;
        // The same number is where a failed attempt's rows end: the retry inside
        // `llm.stream` sends this window, so what a failed attempt wrote after it
        // is dropped before the next one. See `docs/notes/server-agent.md`.
        let window = source.upto_seq;
        // One number for "how long the model took": the whole stream, retries
        // inside it included, because that is what the wall clock paid.
        let requested = std::time::Instant::now();
        // SPEC's ttft and tps both count from the first token: the wait for it,
        // then what came out over the time after.
        let mut first_token: Option<std::time::Instant> = None;
        phases.llm_started = Some(requested);
        let mut rx = llm.stream(
            &prompt.instructions,
            prompt.index.as_deref(),
            prompt.tail.as_deref(),
            prompt.handover.as_deref(),
            &source,
            &definitions,
            effort,
            cancel.clone(),
        );

        // Which call each streamed item is, so its chunks are addressed by call id.
        let mut calls: std::collections::HashMap<String, String> = Default::default();
        // The parts already handed to the front-end as blocks. A refusal can land
        // mid-item, and a block left open there would take the next answer's text
        // for its own.
        let mut began: std::collections::HashSet<(String, ResponsePart)> = Default::default();
        // The items whose content the reader has already seen, so the failure line
        // can say that fragment is being withdrawn.
        let mut said: std::collections::HashSet<String> = Default::default();
        let mut ran_tools = false;
        let mut failure: Option<String> = None;
        let mut saw_completion = false;

        while let Some(event) = rx.recv().await {
            match event {
                // A call's chunks name the id its result carries, so the block it
                // streams into is the one the structured event fills.
                LlmEvent::PartStart {
                    part: ResponsePart::ToolCall,
                    item_id,
                    call_id,
                } => {
                    if let Some(call_id) = call_id {
                        calls.insert(item_id, call_id);
                    }
                }
                LlmEvent::PartStart { .. } => {}
                LlmEvent::Delta {
                    part,
                    item_id,
                    text,
                } => {
                    first_token.get_or_insert_with(std::time::Instant::now);
                    let call_id = calls.get(&item_id).cloned();
                    began.insert((item_id.clone(), part));
                    said.insert(item_id.clone());
                    self.bus.publish(Event::Response(ResponseEvent {
                        session: name.to_string(),
                        turn,
                        part,
                        text,
                        done: false,
                        call_id,
                    }));
                }
                LlmEvent::PartDone { part, item_id } => {
                    let call_id = calls.get(&item_id).cloned();
                    began.insert((item_id.clone(), part));
                    self.bus.publish(Event::Response(ResponseEvent {
                        session: name.to_string(),
                        turn,
                        part,
                        text: String::new(),
                        done: true,
                        call_id,
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
                    said.insert(item_id.clone());
                    let _ = self.store.append_message(id, "reasoning", &payload);
                }
                // The provider executed this one; keeping the item verbatim is
                // what lets the next request carry the same transcript.
                LlmEvent::BuiltinCall {
                    item, replayable, ..
                } => {
                    said.insert(item["id"].as_str().unwrap_or_default().to_string());
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
                        call_id: None,
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
                // Run it here, not after the stream: the call in flight is then
                // always the last row.
                LlmEvent::ToolCall {
                    call_id,
                    name: tool,
                    arguments,
                    ..
                } => {
                    // Before it runs: this id is what the result carries, which
                    // is how a front-end holds a call and its output as one item.
                    self.bus.publish(Event::ToolCall(protocol::ToolCallEvent {
                        session: name.to_string(),
                        call_id: call_id.clone(),
                        head: call_head(&tool, &arguments),
                    }));
                    let _ = self.store.append_message(
                        id,
                        "tool_call",
                        &json!({"call_id": call_id, "name": tool, "arguments": arguments,
                                "step": step}),
                    );
                    let _ = self.store.start_tool_call(id, &call_id, &tool, &arguments);
                    if self
                        .run_one_call(
                            session, id, name, turn, step, cancel, llm, call_id, tool, arguments,
                        )
                        .await
                    {
                        return Ok(StepResult::Stopped);
                    }
                    ran_tools = true;
                }
                LlmEvent::Attempt { probe } => {
                    let mut payload = probe.to_json();
                    payload["step"] = json!(step);
                    payload["turn"] = json!(turn);
                    self.log_event(Some(id), "llm_attempt", &payload);
                }
                LlmEvent::Completed { usage } => {
                    saw_completion = true;
                    // Outside the guard: a provider with no counts still took the time.
                    let ttft_ms =
                        first_token.map(|t| t.duration_since(requested).as_millis() as u64);
                    let streamed = first_token.map(|t| t.elapsed().as_secs_f64());
                    let tps = match (usage.as_ref().and_then(|u| u.output_tokens), streamed) {
                        (Some(out), Some(secs)) if secs > 0.0 => Some(out as f64 / secs),
                        _ => None,
                    };
                    if let Some(u) = &usage {
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
                        self.log_event(
                            Some(id),
                            "usage",
                            &json!({
                                "model": llm.model(),
                                "step": step,
                                "latency_ms": requested.elapsed().as_millis() as u64,
                                "input": u.input_tokens,
                                "cached": cached,
                                "output": u.output_tokens,
                                "total": u.total_tokens,
                                "ttft_ms": ttft_ms,
                                "tps": tps,
                            }),
                        );
                    }
                    // The provider's own counts, never computed: a request that
                    // reported none keeps the last one it did.
                    let context = usage
                        .as_ref()
                        .and_then(|u| u.input_tokens)
                        .or_else(|| self.store.context_tokens(id).ok())
                        .unwrap_or(0);
                    // A share of one request's context says nothing about the next
                    // one's, so an unreported count is not carried over.
                    let cached = usage.as_ref().and_then(|u| u.cached_tokens());
                    let measured = protocol::Usage {
                        context_tokens: context,
                        cached_tokens: cached,
                        tps,
                        ttft_ms,
                    };
                    let _ = self
                        .store
                        .set_usage_for_generation(id, generation, measured);
                    // SPEC: the row updates at every step's end. Off the turn's
                    // path, because reading an allowance is a network call.
                    crate::quota::publish(
                        self.bus.clone(),
                        attempt.llm.provider().map(str::to_string),
                    );
                    // A delta, not a whole list: republishing that every step
                    // would cost every view its scroll.
                    self.bus.publish(Event::Usage(protocol::UsageEvent {
                        session: name.to_string(),
                        usage: measured,
                    }));
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

        // Nothing follows a failure: `llm.stream` retries what another attempt
        // could fix, so what is left here said the request itself is wrong and
        // the fragments this attempt streamed are not its answer.
        if let Some(message) = failure {
            for (item_id, part) in began {
                let call_id = calls.get(&item_id).cloned();
                self.bus.publish(Event::Response(ResponseEvent {
                    session: name.to_string(),
                    turn,
                    part,
                    text: String::new(),
                    done: true,
                    call_id,
                }));
            }
            let discarded = match self.store.truncate_after(id, window) {
                Ok(count) => count,
                Err(e) => {
                    tracing::warn!(session = %name, step, error = %e, "a failed attempt's rows stayed");
                    0
                }
            };
            // One line for the operator, in the voice the retries already use: a
            // refusal is news, and taken-for-an-answer text is never resumed.
            self.bus.publish(Event::Log {
                level: LogLevel::Warn,
                message: failed_warning(&name, &message, &said, discarded),
            });
            anyhow::bail!("{message}");
        }
        if !saw_completion {
            anyhow::bail!("model stream ended without completing");
        }
        if cancel.is_cancelled() {
            return Ok(StepResult::Stopped);
        }
        if !ran_tools {
            return Ok(StepResult::Finished);
        }
        tracing::debug!(session = %name, step, "step ran its calls as they arrived");
        Ok(StepResult::RanTools)
    }

    /// Turns the `@image:` references in a tool's output into attachments.
    ///
    /// # Invariant
    ///
    /// A reference that names no readable picture is left as text, so a command
    /// printing one cannot fail the turn; a model that cannot take images is told
    /// so once, in the output, rather than having them silently dropped.
    async fn attach_references(
        &self,
        ctx: &ToolContext,
        output: &str,
        llm: &LlmClient,
    ) -> Vec<crate::image::ImageRef> {
        let mut out = Vec::new();
        // The bytes are here; the picture is on the peer, so a name is not a file.
        let on_endpoint = ctx.on_endpoint();
        let named = crate::meta::mentions(output)
            .into_iter()
            .filter(|m| m.kind == protocol::Reference::Image)
            .map(|m| m.value);
        for given in named {
            if on_endpoint {
                self.bus.publish(Event::Log {
                    level: LogLevel::Warn,
                    message: format!(
                        "{}: {given} stays a reference in the text -- this session's pictures \
                         are on its remote endpoint, and reading them into this machine is \
                         not available in this build",
                        ctx.session
                    ),
                });
                continue;
            }
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

    /// Runs the one call the model just asked for. Returns true if the turn was cancelled.
    async fn run_one_call(
        &self,
        session: &SessionHandle,
        id: i64,
        name: &str,
        turn: u64,
        step: u32,
        cancel: &tokio_util::sync::CancellationToken,
        llm: &LlmClient,
        call_id: String,
        tool: String,
        arguments_given: String,
    ) -> bool {
        let started_at = crate::store::now_millis();
        let started = std::time::Instant::now();
        if cancel.is_cancelled() {
            return true;
        }
        let arguments: serde_json::Value =
            serde_json::from_str(&arguments_given).unwrap_or_else(|_| json!({}));

        // Forward streamed tool output onto the bus, tagging each chunk with
        // the tool name the context does not know.
        let (tx, mut rx) = mpsc::channel::<ToolAnswerEvent>(256);
        let bus = self.bus.clone();
        let tool_label = tool.clone();
        // Whether the window heard anything from the tool itself.
        let sent = Arc::new(AtomicUsize::new(0));
        let forwarded = Arc::clone(&sent);
        let forwarder = tokio::spawn(async move {
            while let Some(mut e) = rx.recv().await {
                e.tool = tool_label.clone();
                forwarded.fetch_add(1, Ordering::Relaxed);
                bus.publish(Event::ToolAnswer(e));
            }
        });

        let ctx = ToolContext {
            session: name.to_string(),
            call_id: call_id.clone(),
            output_dir: self.output_dir.clone(),
            shells: session.shells(),
            detached: self.detached.clone(),
            events: Some(tx),
            cancel: cancel.clone(),
            store: self.store.clone(),
            cwd: session.cwd(),
            vision: llm.vision(crate::prompts::shared()),
            inbox: session.inbox_signal(),
            model: llm.model().to_string(),
            price: self.price,
            cache_invalid_rate: self.cache_invalid_rate,
            cache_ttl_ms: self.cache_ttl_ms,
            closing: session.closing_signal(),
        };
        let run_started = std::time::Instant::now();
        tracing::info!(session = %name, tool = %tool, "tool start");
        let outcome = self.tools.run(&tool, arguments, &ctx).await;
        let run_ms = run_started.elapsed().as_millis() as u64;
        let attach_started = std::time::Instant::now();
        let scanned = self.attach_references(&ctx, &outcome.output, llm).await;
        let attach_ms = attach_started.elapsed().as_millis() as u64;
        // A tool that streamed nothing owes its output to the final chunk.
        let tail = match sent.load(Ordering::Relaxed) {
            0 => outcome.output.clone(),
            _ => String::new(),
        };
        // The model is shown no head and no label, so the time ends the text it reads.
        let output = with_time(outcome.output, run_ms);
        drop(ctx);
        // A shell on an endpoint starts in a directory only that endpoint knows.
        session.adopt_reported_cwd();
        let _ = forwarder.await;
        tracing::info!(
            session = %name,
            tool = %tool,
            ok = outcome.ok,
            elapsed = ?started.elapsed(),
            "tool end"
        );

        // The one chunk that ends the call, and the only one that knows how it went.
        self.bus.publish(Event::ToolAnswer(ToolAnswerEvent {
            session: name.to_string(),
            call_id: call_id.clone(),
            tool: tool.clone(),
            stream: ToolStream::Stdout,
            text: tail,
            done: true,
            head: Some(result_head(outcome.ok, run_ms)),
        }));

        let status = if outcome.ok { "ok" } else { "error" };
        // A command that printed `@image: <path>` names a picture: it becomes an
        // attachment, uploaded once per file.
        let images: Vec<_> = scanned
            .iter()
            .map(crate::image::ImageRef::to_json)
            .collect();
        let _ = self.store.append_message(
            id,
            "tool_result",
            &json!({"call_id": call_id, "output": output, "images": images,
                    "step": step, "ok": outcome.ok, "elapsed_ms": run_ms}),
        );
        let _ = self.store.finish_tool_call(id, &call_id, &output, status);
        self.log_event(
            Some(id),
            "tool_result",
            &json!({"tool": tool, "call_id": call_id, "status": status}),
        );
        self.log_event(
            Some(id),
            "tool_timing",
            &json!({
                "turn": turn,
                "step": step,
                "tool": tool,
                "call_id": call_id,
                "started_at": started_at,
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "run_ms": run_ms,
                "attach_ms": attach_ms,
                "arguments_chars": arguments_given.chars().count(),
                "output_chars": output.chars().count(),
                "status": status,
            }),
        );
        false
    }

    /// Reports a failure, and leaves the reason somewhere it can be read later.
    ///
    /// # Invariant
    ///
    /// The line goes into the transcript as a `notice`, which no request is built
    /// from. An `Event::Log` alone is broadcast and forgotten: "why did that
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

#[derive(Default)]
struct StepPhases {
    prompt_ms: u64,
    source_ms: u64,
    llm_started: Option<std::time::Instant>,
}

enum StepResult {
    /// The model answered without asking for tools.
    Finished,
    /// The model asked for tools, and this step ran each one as it arrived.
    RanTools,
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

impl TurnOutcome {
    /// The word a durable row uses, so a reader never parses a debug format.
    fn label(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// What a panicking task carries, as text.
pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> String {
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

/// The operator-facing line for a request no further attempt can fix.
///
/// # Invariant
///
/// Never empty and never silent about what the attempt already said: a failed
/// attempt that streamed part of an answer leaves text the reader has seen and
/// the transcript does not keep, and the line has to say so or the gap is
/// unexplained. See `docs/notes/server-agent.md`.
fn failed_warning(
    session: &str,
    reason: &str,
    said: &std::collections::HashSet<String>,
    discarded: usize,
) -> String {
    let mut line = format!("{session}: model request failed and will not be retried ({reason})");
    if !said.is_empty() {
        line.push_str("; the partial answer this attempt streamed is discarded");
    }
    if discarded > 0 {
        line.push_str(&format!(" ({discarded} row(s) the attempt wrote dropped)"));
    }
    line
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
) -> String {
    prompts.render(
        "context_notice",
        &[
            ("session", session),
            ("tokens", &tokens.to_string()),
            ("budget", &budget.to_string()),
        ],
    )
}

/// The notice threshold in input tokens, or 0 for "off".
fn notice_budget(configured: u64, context: Option<u64>, percent: Option<u8>) -> u64 {
    if configured == 0 {
        return 0;
    }
    let Some(window) = context.filter(|w| *w > 0) else {
        return configured;
    };
    providers::tokens_for_percent(window, percent.unwrap_or(DEFAULT_CONTEXT_LIMIT_PERCENT))
}

/// What a call is, and the words it is read with: the tool and its arguments.
///
/// # Invariant
///
/// Kind and text only -- never a glyph or a colour. This crate knows what a tool
/// is; how that looks belongs to the front-end, so restyling is a change to one
/// crate and nothing on the wire moves. See `docs/notes/protocol-message.md`.
pub(crate) fn call_head(tool: &str, arguments: &str) -> protocol::Head {
    protocol::Head::call(tool, call_fields(tool, arguments))
}

/// A call's arguments, each marked with whether SPEC's `abstract` shows it.
///
/// # Invariant
///
/// The per-tool list is this crate's: it owns the tools, so it knows a `bash`
/// call is summarised by `{name, task, background, detached, timeout}` and that
/// `command` is the detail's. The wire carries the verdict per field, so adding a
/// tool touches nothing a front-end reads. Every argument is sent once: what the
/// summary leaves out is here, so opening needs no request.
fn call_fields(tool: &str, arguments: &str) -> Vec<protocol::Field> {
    let value: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let summary = summary_keys(tool);
    // SPEC's `toolcall{tool_name, ...}`: the name rides on the head, where it is
    // the one part of the summary that is always there.
    let mut fields = Vec::new();
    // The detail first, so opening the item leads with what was run.
    if let Some(command) = value.get("command").and_then(|v| v.as_str()) {
        fields.push(protocol::Field {
            label: String::new(),
            value: command.to_string(),
            summary: false,
        });
    }
    if value.as_object().is_none() {
        return fields;
    }
    // SPEC's order first, so the row reads the same whatever the model wrote;
    // then anything else, in the order it came.
    let mut ordered: Vec<String> = summary
        .iter()
        .map(|name| tool_key(name).to_string())
        .filter(|key| value.get(key).is_some())
        .collect();
    if let Some(map) = value.as_object() {
        for key in map.keys() {
            if key != "command" && !ordered.contains(key) {
                ordered.push(key.clone());
            }
        }
    }
    for key in ordered {
        let label = spec_name(&key).to_string();
        fields.push(protocol::Field {
            value: match &value[&key] {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            },
            summary: summary.contains(&label.as_str()),
            label,
        });
    }
    fields
}

/// The arguments SPEC's `toolcall` summarises this tool by, in its order.
fn summary_keys(tool: &str) -> &'static [&'static str] {
    match tool {
        // SPEC lists bash's own set; another tool is summarised by nothing yet,
        // which still reads better than a wall of JSON.
        "bash" => &["name", "task", "background", "detached", "timeout"],
        _ => &[],
    }
}

/// The name the tool's own schema uses, where SPEC spells it otherwise.
fn tool_key(name: &str) -> &str {
    match name {
        "timeout" => "timeout_ms",
        other => other,
    }
}

/// The name SPEC gives an argument, where the tool's own schema spells it otherwise.
fn spec_name(key: &str) -> &str {
    match key {
        "timeout_ms" => "timeout",
        other => other,
    }
}

/// What a finished call says: whether it worked, and how long it took.
///
/// # Invariant
///
/// The duration is spelled here because only this crate measured it; the mark
/// and the colour that go beside it are the front-end's.
pub(crate) fn result_head(ok: bool, run_ms: u64) -> protocol::Head {
    let kind = if ok {
        protocol::HeadKind::Ok
    } else {
        protocol::HeadKind::Failed
    };
    protocol::Head::new(kind, spent(run_ms))
}

/// `820ms`, or `1.2s` past a second: one duration, one spelling, every reader.
pub(crate) fn spent(millis: u64) -> String {
    match millis {
        0..1000 => format!("{millis}ms"),
        _ => format!("{:.1}s", millis as f64 / 1000.0),
    }
}

/// A call's output as the model receives it: what the tool said, then how long it took.
///
/// # Invariant
///
/// The same text is stored, so a read shows the line the run produced and the model's
/// next request carries the number. See `docs/notes/server-agent.md`.
fn with_time(output: String, run_ms: u64) -> String {
    let note = format!("[elapsed {}]", spent(run_ms));
    if output.is_empty() || output.ends_with('\n') {
        format!("{output}{note}")
    } else {
        format!("{output}\n{note}")
    }
}
