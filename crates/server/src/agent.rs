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
    TurnOutcome,
};
use serde_json::json;
use tokio::sync::mpsc;

use crate::bus::EventBus;
use crate::llm::{LlmClient, LlmEvent};
use crate::providers::{self};
use crate::session::SessionHandle;
use crate::store::{Kind, Store};
use crate::tools::{ToolContext, ToolRegistry};

/// A turn that failed because the provider refused the request or stopped answering.
///
/// # Invariant
///
/// The origin of a failure rides the error, not its wording: the monitor that
/// runs on a provider failure is the `providers` one, and the monitor that runs on
/// anything else is none of its business. Reading the reason out of a message would
/// make a wording change a routing change.
#[derive(Debug)]
pub struct ProviderFailure(pub String);

/// How many of one turn's requests the provider's inspection may refuse before it stops.
const MODERATION_PER_TURN: u32 = 3;

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProviderFailure {}

pub struct Agent {
    pub store: Store,
    /// The machine's roster, which `--model` names resolve against. A live view:
    /// a reload reaches every later route.
    pub providers: providers::Handle,
    /// Clients for sessions that named their own model, by `provider/model_name`.
    ///
    /// # Invariant
    ///
    /// Keyed by the roster read and the config read that produced each one: a cached
    /// client carries the base URL, the key, the timeouts and the model it was built
    /// from, so one kept across a reload sends the new model to the old endpoint, and
    /// one kept across an edit sends the timeout the file no longer states.
    pub clients: std::sync::Mutex<std::collections::HashMap<String, (u64, u64, Arc<LlmClient>)>>,
    /// The tool table the config file describes, rebuilt when the file is read again.
    /// See `crate::live::Derived`.
    pub tools: crate::live::Derived<crate::config::Config, Arc<ToolRegistry>>,
    /// The channels `exec` speaks to, and what a context is built with.
    pub channels: Arc<crate::tools::channel::Channels>,
    pub bus: EventBus,
    /// The home this call belongs to: what a worker needs to be started.
    pub home: PathBuf,
    /// The home's config file as it is now: what a step reads, instead of what the
    /// process started with.
    pub config: Arc<crate::config::Current>,
    /// Where capped tool output is spilled; without it the tail is lost.
    pub output_dir: PathBuf,
}

impl Agent {
    /// The tool table the config file describes now.
    ///
    /// # Invariant
    ///
    /// One table per config read: a call is looked up in the table the file stated
    /// when the call was made, and a head is stamped from it, so a `[tool.<name>]`
    /// edit reaches the next call and the next session without a restart.
    pub fn tools(&self) -> Arc<ToolRegistry> {
        let config = self.config.now();
        self.tools.of(config.read, &config)
    }

    /// Writes one row to the log and announces it, in that order.
    ///
    /// # Invariant
    ///
    /// The log is the source of truth and the bus its notification: the row is on
    /// disk before anybody is told, and every row the agent writes goes through
    /// here, so a subscriber can always read what it was told about.
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
    ///
    /// # Invariant
    ///
    /// The gate stays held until the ending is written: an interrupt ends a call wait,
    /// and the notice about that call wakes the session at once. A gate free when
    /// `turn_inner` returned let that wake start the next turn inside the window, so
    /// its row was saved before the text the cancel preserved. See
    /// `docs/notes/server-agent.md`.
    pub async fn run_turn(
        &self,
        session: &SessionHandle,
        tasks: Vec<i64>,
        epoch: u64,
        gate: tokio::sync::OwnedMutexGuard<()>,
    ) -> (TurnOutcome, bool) {
        let started = std::time::Instant::now();
        // Dropped last, after the ending exists: nothing may start the next turn first.
        let _gate = gate;
        let outcome = AssertUnwindSafe(self.turn_inner(session, epoch))
            .catch_unwind()
            .await;
        let (outcome, steps, origin) = match outcome {
            Ok(triple) => triple,
            Err(payload) => {
                let message = panic_message(&payload);
                tracing::error!(session = %session.name(), "turn panicked: {message}");
                self.write_error(
                    session.id(),
                    session.name(),
                    &format!("{}: turn panicked: {message}", session.name()),
                );
                (TurnOutcome::Failed, 0, Some("harness"))
            }
        };
        // Captured before anything else: a next turn may already be resetting the flags.
        let interrupted = session.interrupted();
        // No part outlives its turn: what is left is text whose row was never written.
        session.open_clear();
        // The turn's own words ride the row, whole: `{event.answer}` is that same text,
        // never a head.
        let answer = self.store.last_answer(session.id()).unwrap_or(None);
        // The turn's outcome as a row, not only a line: a reader with the database need
        // not match prose.
        let mut row = serde_json::json!({
            "outcome": outcome,
            "steps": steps,
            // Why it failed, for the monitor that reads this row back: a
            // provider's refusal is the `providers` monitor's business.
            "origin": origin,
            "elapsed_ms": started.elapsed().as_millis() as u64,
        });
        // A stopped turn did not end, so it does not say it did: see server-agent.md.
        let kind = match outcome {
            TurnOutcome::Cancelled => "turn_interrupted",
            _ => {
                row["answer"] = serde_json::json!(answer);
                "turn_end"
            }
        };
        self.log_event(Some(session.id()), kind, &row);
        let (status, detail) = match outcome {
            TurnOutcome::Done => (SessionStatus::Idle, None),
            // SPEC's split: cancel cancels and a restart continues; interrupt ends it.
            TurnOutcome::Cancelled if interrupted => {
                (SessionStatus::Idle, Some("interrupted".to_string()))
            }
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
                outcome: Some(outcome),
            });
        }
        (outcome, interrupted)
    }

    async fn turn_inner(
        &self,
        session: &SessionHandle,
        epoch: u64,
    ) -> (TurnOutcome, u32, Option<&'static str>) {
        let name = session.name().to_string();
        let id = session.id();
        if session.cancel_epoch() != epoch {
            // Cancelled while queued: running looks like the interrupt did nothing.
            tracing::info!(session = %name, "turn dropped: cancelled while queued");
            return (TurnOutcome::Cancelled, 0, None);
        }
        if session.is_deleted() {
            tracing::info!(session = %name, "turn dropped: session was deleted before starting");
            return (TurnOutcome::Cancelled, 0, None);
        }
        let (turn, cancel) = session.new_turn();
        let started = std::time::Instant::now();

        self.bus.publish(Event::Status {
            session: name.clone(),
            status: SessionStatus::Running,
            detail: None,
            outcome: None,
        });
        let _ = self.store.set_status(id, SessionStatus::Running);

        // No step ceiling: it ends on a stop, a cancel or a failure, and every break sets it.
        let mut step: u32 = 0;
        // The step whose refusal was already answered with a row.
        let mut repaired: Option<u32> = None;
        // Read once, so a turn sends one tier on every step; a set_session that
        // lands mid-turn is meant for the next one.
        let stated_tier = session.tuning().cache_tier;

        // Inspection refusals this turn has absorbed; see `docs/notes/server-llm.md`.
        let moderation = std::sync::atomic::AtomicU32::new(0);
        let outcome = loop {
            // A `cancel` ends the turn here, where it can be stored whole.
            if cancel.is_cancelled() {
                break (TurnOutcome::Cancelled, step, None);
            }
            // Step boundary: a user line between a call and its result is rejected, so inject here.
            self.deliver_inbox(id, &name);
            // Read here every step: a `set_session` landing mid-turn is meant for the
            // next request.
            let (llm, effort, budget, window) = match self.settings(&name, session) {
                Ok(settings) => settings,
                Err(e) => {
                    // A model that is not in the roster or has no key is the roster's
                    // business, which is the same monitor as a refusal.
                    self.error(id, &name, "resolving the session's provider", e);
                    break (TurnOutcome::Failed, step, Some("provider"));
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
            self.maybe_notify_context(session, id, &name, budget, window);
            self.maybe_request_live_compact(session, id, &name);
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
                tier: stated_tier.as_deref(),
                moderation: &moderation,
            };
            match self.step(session, id, &name, turn, &cancel, &attempt).await {
                Ok(StepResult::Finished) => {
                    // Something may have landed while it finished; deliver it and go round again.
                    if self.deliver_inbox(id, &name) > 0 {
                        step = step.wrapping_add(1);
                        continue;
                    }
                    break (TurnOutcome::Done, step, None);
                }
                Ok(StepResult::RanTools) => {}
                Ok(StepResult::Stopped) => break (TurnOutcome::Cancelled, step, None),
                // A refusal a row can answer is answered once, then asked again.
                Err(e)
                    if repaired != Some(step)
                        && crate::llm::thinking_refusal(&format!("{e:#}")) =>
                {
                    repaired = Some(step);
                    self.append_thinking_mark(id, &name);
                    continue;
                }
                Err(e) => {
                    let origin = if e.downcast_ref::<ProviderFailure>().is_some() {
                        "provider"
                    } else {
                        "harness"
                    };
                    self.error(id, &name, "model request", e);
                    break (TurnOutcome::Failed, step, Some(origin));
                }
            }
            // Wrapping, not saturating: the counter is only a log label.
            step = step.wrapping_add(1);
        };

        // Interrupt: fold the inbox into context while the gate is held, so nudge finds nothing to wake.
        if matches!(outcome.0, TurnOutcome::Cancelled) && session.interrupted() {
            self.deliver_inbox(id, &name);
        }
        tracing::info!(session = %name, outcome = ?outcome.0, elapsed = ?started.elapsed(), "turn end");
        outcome
    }

    /// Where the next request sends: the model, the level and the notice budget, all
    /// read from the session as it is now.
    fn settings(
        &self,
        name: &str,
        session: &SessionHandle,
    ) -> anyhow::Result<(Arc<LlmClient>, Option<String>, u64, Option<u64>)> {
        let tuning = session.tuning();
        let (llm, budget, window) = self.route(name, &tuning)?;
        Ok((llm, tuning.thinking_effort, budget, window))
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
    ) -> anyhow::Result<(Arc<LlmClient>, u64, Option<u64>)> {
        let (llm, context) = match &tuning.model {
            // The config's `[model]` is the server's own, never a session's.
            None => anyhow::bail!("{name} states no model: set_session <session> <model>"),
            Some(model) => {
                let choice = self
                    .providers
                    .resolve(model)
                    .or_else(|e| self.providers.resolve_by_name(model).ok_or(e))
                    .map_err(|e| anyhow::anyhow!("--model {model}: {e}"))?;
                let client = self.client_for(&choice.entry, choice.api_key)?;
                let mode = tuning
                    .live_compact_mode
                    .unwrap_or(self.config.now().agent.live_compact_mode);
                let context = if mode == crate::live_compact::CompactMode::Mask {
                    choice.entry.unmasked_context.or(choice.entry.context)
                } else {
                    choice.entry.context
                };
                (client, context)
            }
        };
        let budget = self.budget(name, context, tuning.context_limit);
        Ok((llm, budget, context))
    }

    /// The visible-token ceiling used for automatic memory displacement.
    pub(crate) fn memory_token_limit(
        &self,
        llm: &LlmClient,
        tuning: &crate::session::Tuning,
    ) -> Option<u64> {
        let context = llm.unmasked_context()?;
        let percent = tuning
            .context_limit
            .unwrap_or(self.config.now().agent.context_limit_percent);
        Some(crate::providers::tokens_for_percent(context, percent))
    }

    /// The notice threshold in input tokens; zero means off without exception.
    fn budget(&self, name: &str, context: Option<u64>, percent: Option<u8>) -> u64 {
        let config = self.config.now();
        // A percentage has no size without a window; warn rather than guess.
        if config.agent.compress_above_tokens != 0 && context.is_none() && percent.is_some() {
            tracing::warn!(
                session = %name,
                "no context window is known for this model; using the configured token budget"
            );
        }
        notice_budget(
            config.agent.compress_above_tokens,
            config.agent.context_limit_percent,
            context,
            percent,
        )
    }

    /// The client for one roster entry, built once and shared.
    fn client_for(
        &self,
        entry: &providers::Entry,
        api_key: String,
    ) -> anyhow::Result<Arc<LlmClient>> {
        let model = entry.model.clone();
        let generation = self.providers.generation();
        let config = self.config.now();
        let read = config.read;
        let cached = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&model)
            .cloned();
        if let Some((pinned, pinned_read, client)) = cached
            && pinned == generation
            && pinned_read == read
        {
            return Ok(client);
        }
        // Built outside the lock: a duplicate client is cheaper than serialising turns.
        let mut cfg = config.model.for_entry(entry, api_key);
        // Every client measures its own provider, into this home's store.
        cfg.measure = Some(self.store.clone());
        cfg.price = entry.price;
        let client = Arc::new(LlmClient::new(&cfg)?.with_unmasked_context(entry.unmasked_context));
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(model, (generation, read, client.clone()));
        Ok(client)
    }

    /// Moves the session's inbox into the transcript in one durable transaction.
    pub(crate) fn deliver_inbox(&self, id: i64, name: &str) -> usize {
        let agent = &self.config.now().agent;
        let live_compact = agent.live_compact_enabled;
        let compact_mode = agent.live_compact_mode;
        match self
            .store
            .deliver_pending(id, name, live_compact, compact_mode)
        {
            // `waited` is the "no answer yet" number: a message written mid-step lands
            // only here.
            Ok((delivered, waited_ms, written)) => {
                if delivered > 0 {
                    self.announce_operator_rows(name, written);
                    tracing::info!(
                        session = %name,
                        delivered,
                        waited = ?std::time::Duration::from_millis(waited_ms.max(0) as u64),
                        "delivered queued messages at a step boundary"
                    );
                    self.bus.publish(Event::Log {
                        level: LogLevel::Info,
                        message: format!("{name}: delivered {delivered} queued message(s)"),
                        session: None,
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
    /// `handoff` skill and `docs/notes/server-agent.md`.
    fn maybe_notify_context(
        &self,
        session: &SessionHandle,
        id: i64,
        name: &str,
        budget: u64,
        window: Option<u64>,
    ) {
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
        if !session.note_context_asked(tokens, budget, window) {
            return;
        }
        // SPEC's Mentor: an archive hears its size and is asked for nothing.
        let message = match session.mentor_of() {
            Some(_) => context_measure(crate::prompts::shared(), name, tokens, budget, window),
            None => context_notice(crate::prompts::shared(), name, tokens, budget, window),
        };
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
            message: match session.mentor_of() {
                Some(_) => format!(
                    "{name}: context at {tokens} of {budget} tokens; told the archive its size"
                ),
                None => format!(
                    "{name}: context at {tokens} of {budget} tokens; asked the agent to fold"
                ),
            },
            session: None,
        });
    }

    /// Asks for a checkpoint once the context has grown a whole interval since the
    /// last one.
    ///
    /// # Invariant
    ///
    /// The interval is the base backed off by every ask that went unanswered, so an
    /// agent that ignores one is asked again a wider stretch later rather than at
    /// every step. The switch, the base and the backoff are all read now: a session
    /// that states none follows the home's config as it is, not as this process
    /// started. See `docs/notes/server-live-compact.md`.
    fn maybe_request_live_compact(&self, session: &SessionHandle, id: i64, name: &str) {
        let tuning = session.tuning();
        let config = self.config.now();
        if !tuning
            .live_compact_enabled
            .unwrap_or(config.agent.live_compact_enabled)
        {
            return;
        }
        let base = tuning
            .live_compact_interval_tokens
            .unwrap_or(config.agent.live_compact_interval_tokens);
        let Ok(measurement) = self.store.live_compact_measurement(id) else {
            return;
        };
        let Some(baseline) = measurement.baseline_tokens else {
            return;
        };
        let interval = measurement.interval.unwrap_or(base);
        if measurement.tokens.saturating_sub(baseline) < interval {
            return;
        }
        let mode = tuning
            .live_compact_mode
            .unwrap_or(config.agent.live_compact_mode);
        let next = interval.saturating_mul(2);
        let message = match mode {
            crate::live_compact::CompactMode::Cut => live_compact_notice(),
            crate::live_compact::CompactMode::Mask => memory_mask_notice(),
        };
        match self.store.request_live_compact(
            id,
            name,
            measurement.generation,
            &message,
            next,
            mode,
        ) {
            Ok(true) => {
                self.log_event(
                    Some(id),
                    "live_compact_notice",
                    &json!({
                        "tokens": measurement.tokens,
                        "baseline": baseline,
                        "interval": interval,
                        "next_interval": next,
                        "generation": measurement.generation,
                        "mode": mode.as_str(),
                    }),
                );
            }
            Ok(false) => {}
            Err(e) => self.error(id, name, "queueing the live compact request", e),
        }
    }

    /// The prose this session sends: stamped once, and the same bytes for ever.
    ///
    /// # Invariant
    ///
    /// The head is part of the cached prefix: written once, then read from the store.
    /// What the references say is read every step, but a change is a row appended, never
    /// a head that moves. See `docs/notes/server-agent.md`.
    async fn prompt_in_force(
        &self,
        session: &SessionHandle,
        id: i64,
        model: &str,
        cwd: &std::path::Path,
        machines: &crate::meta::Machines<'_>,
    ) -> anyhow::Result<crate::store::Prompt> {
        // All three "where"s come from the session: its name, a stated placement, the
        // peer.
        let name = session.name();
        let placed = session.placed();
        let placed = placed.as_deref();
        let remote = session.remote_name();
        let remote = remote.as_deref();
        // Where it stands now, which is what the card and the plan are about.
        let here = match placed {
            Some(path) => crate::session::stated(path),
            None => crate::session::stated(cwd),
        };
        // The first row is the head: storage order is send order, and `prompt` reports a
        // head it cannot read, never `None`.
        if let Some(kept) = self.store.prompt(id)? {
            return Ok(kept);
        }
        let prompts = crate::prompts::shared();
        // The prose carries no value at all: the card below states the name, the
        // build, the first minute and the directory.
        let instructions = prompts.system();
        let home = std::path::PathBuf::from(&here);
        let tail = Some(prompts.render(
            // One shape per fact a card states: a template has no conditional.
            match (remote, here.is_empty()) {
                (Some(_), false) => crate::prompts::TAIL_REMOTE,
                (Some(_), true) => crate::prompts::TAIL_REMOTE_UNSTATED,
                (None, _) => crate::prompts::TAIL,
            },
            &[
                ("session", name),
                ("build", protocol::BUILD),
                ("now", &crate::clock::now()),
                ("cwd", &here),
                ("remote", remote.unwrap_or_default()),
                ("model", model),
            ],
        ));
        // This row states no section: the tool row carries it, and a request sends it there.
        let parameters = None;
        // The references' plan, stamped once; an opted-out session gets no plan row.
        let plan_on = session
            .tuning()
            .prefix_cache_plan
            .unwrap_or(self.config.now().agent.prefix_cache_plan);
        let (plan, rooted, plan_explicit, plan_cached, agents_md) = if plan_on {
            match self
                .plan_of(id, name, model, &here, &home, machines, remote)
                .await
            {
                Some(parts) => (Some(parts.0), parts.1, parts.2, parts.3, parts.4),
                None => (None, Vec::new(), Vec::new(), Vec::new(), None),
            }
        } else {
            (None, Vec::new(), Vec::new(), Vec::new(), None)
        };
        let prompt = crate::store::Prompt {
            instructions,
            plan,
            agents_md,
            tail,
            parameters,
            rooted,
            plan_explicit,
            plan_cached,
            cwd: here,
            remote: remote.map(str::to_string),
            model: Some(model.to_string()),
        };
        // The store refuses a second head: reading the head in force is how a race answers
        // instead of losing a request.
        match self.store.append_prompt(id, &prompt) {
            Ok(_) => Ok(prompt),
            Err(_) => self
                .store
                .prompt(id)?
                .ok_or_else(|| anyhow::anyhow!("session {id} lost the head it was about to write")),
        }
    }

    /// The stored tool set this session sends, written once and only read after.
    ///
    /// # Invariant
    ///
    /// The head is written once and only read afterwards.
    ///
    /// # Invariant
    ///
    /// A schema is context, and context has two operations: append and cut. A
    /// schema is neither, so it is written when the session first asks and never
    /// again -- Store::write_head_once refuses a second write, so no caller can
    /// express one. Settings are not context: set_session may change the model,
    /// the peer or the mode freely and none of them re-renders these bytes.
    fn tool_set_in_force(
        &self,
        session: &SessionHandle,
        id: i64,
        model: &str,
        llm: &crate::llm::LlmClient,
    ) -> anyhow::Result<crate::store::ToolSet> {
        if let Some(kept) = self.store.tool_set(id)? {
            return Ok(kept);
        }
        // One section per head: a head whose info row has it keeps it there.
        let older = self.store.prompt(id)?;
        let tool_set = crate::store::ToolSet {
            tools: llm.wire_tools(&self.tools().stamp()),
            parameters: match older.and_then(|prompt| prompt.parameters) {
                Some(_) => None,
                None => self.shared_arguments(),
            },
            model: Some(model.to_string()),
            remote: session.remote_name(),
            build: protocol::BUILD.to_string(),
            event: "first".to_string(),
        };
        self.store.write_head_once(id, &tool_set)?;
        Ok(tool_set)
    }

    /// The section about the arguments every call shares, where it is sent.
    ///
    /// # Invariant
    ///
    /// One template, read once per head: it belongs to the tool row rather than to the
    /// prompt, because its words are about a call's arguments. A home whose content has
    /// none sends no section rather than refusing to start.
    fn shared_arguments(&self) -> Option<String> {
        crate::prompts::shared()
            .text(crate::prompts::PARAMETERS)
            .filter(|text| !text.is_empty())
    }

    /// How much a planned entry may change before it is re-sent, or nothing when
    /// no patch may be appended.
    ///
    /// # Invariant
    ///
    /// A patch is appended only where the prose introducing it exists: a diff with no
    /// template behind it would arrive as an unexplained second copy of the entry.
    fn plan_tolerance(&self) -> Option<f64> {
        let prompts = crate::prompts::shared();
        let explained = prompts.text(crate::prompts::REVISION).is_some();
        explained
            .then(|| self.config.now().agent.plan_change_tolerance)
            .filter(|tolerance| *tolerance > 0.0)
    }

    /// How many tokens `text` is worth on the roster model named `model`.
    fn tokens_on(&self, model: &str, text: &str) -> usize {
        let ratio = self.store.token_ratio(model);
        (text.len() as f64 * ratio).round().max(1.0) as usize
    }

    /// The tier every request of this session sends.
    ///
    /// # Invariant
    ///
    /// A session that named its own tier keeps it; otherwise the server's
    /// configured tier wins, and only then does the roster's shortest tier
    /// answer. Only a tier the roster declares may be sent, so an ignored
    /// wire field is never priced.
    fn cache_tier(&self, llm: &LlmClient, stated: Option<&str>) -> Option<String> {
        // Stated `none`: neither the server's tier nor the roster's shortest answers.
        if stated == Some(protocol::NONE_VALUE) {
            return None;
        }
        let entry = self.providers.place(llm.model());
        let tiers = entry.as_ref().map(|entry| &entry.cache_ttl_tiers);
        if let Some(tier) = stated.filter(|t| tiers.is_some_and(|m| m.contains_key(*t))) {
            return Some(tier.to_string());
        }
        if let Some(tier) = llm.cache_tier() {
            return Some(tier.to_string());
        }
        let picked = tiers?.iter().min_by_key(|(_, secs)| **secs)?;
        Some(picked.0.clone())
    }

    /// How far into the tier's window the interrupt and the next heartbeat fire.
    ///
    /// # Invariant
    ///
    /// The margin is a share of the stated window, not an invented grace period:
    /// firing at the last instant would let lazy eviction win the race. A session
    /// that named its own tier times against that tier's own window.
    pub(crate) fn cache_fire_after_ms(&self, model: &str, tier: Option<&str>) -> u64 {
        let pct = 100_u64.saturating_sub(self.config.now().model.cache_margin_pct as u64);
        self.cache_window_ms(model, tier).saturating_mul(pct) / 100
    }

    /// The tier's whole window: the upper edge of the heartbeat's band.
    ///
    /// # Invariant
    ///
    /// The same roster lookup that places the fire point also places the
    /// expiry; one model, one tier, one window for both edges of the band.
    pub(crate) fn cache_window_ms(&self, model: &str, tier: Option<&str>) -> u64 {
        let entry = self.providers.place(model);
        let config = self.config.now();
        tier.and_then(|name| entry.as_ref().and_then(|e| e.cache_ttl_tiers.get(name)))
            .map(|secs| secs.saturating_mul(1_000))
            .or(cache_ttl_ms(&config))
            .or_else(|| {
                entry
                    .as_ref()
                    .and_then(|e| e.cache_ttl_secs)
                    .map(|secs| secs.saturating_mul(1_000))
            })
            .unwrap_or(0)
    }

    /// What a plan for `model` is priced at.
    ///
    /// # Invariant
    ///
    /// The roster answers for everything it states: the price and the lifetime are
    /// the model's provider's, never another model's. This machine's own lifetime
    /// applies only when its config states one, and a provider that states neither
    /// leaves it at zero, which the plan reads as "never expires".
    pub(crate) fn rates(&self, model: &str) -> crate::prefix_cache::Rates {
        let entry = self.providers.place(model);
        let stated = entry
            .as_ref()
            .and_then(|entry| entry.cache_ttl_secs)
            .map(|secs| secs.saturating_mul(1_000));
        // A measured rate wins; the configured one answers until there is a row.
        let observed = self
            .store
            .cache_hit_posterior(model)
            .ok()
            .flatten()
            .map(|rate| rate.clamp(0.0, 1.0));
        let config = self.config.now();
        crate::prefix_cache::Rates {
            price: entry.as_ref().and_then(|entry| entry.price),
            invalid: observed.unwrap_or(config.agent.cache_invalid_rate),
            ttl_ms: cache_ttl_ms(&config).or(stated).unwrap_or(0),
        }
    }

    /// The index this request carries: what every reference says now, cheapest first.
    ///
    /// # Invariant
    ///
    /// The order is `prefix_cache`'s, a permutation of what it was given: no entry is
    /// invented for a reference that could not be read, and the spellings returned are the
    /// ones the plan carries, so a message naming one again keys the same entry. The rules
    /// travel outside it (`meta::rules`). Everything is read in `home`. See
    /// `docs/notes/server-agent.md`.
    async fn plan_of(
        &self,
        id: i64,
        name: &str,
        model: &str,
        owner: &str,
        home: &std::path::Path,
        machines: &crate::meta::Machines<'_>,
        remote: Option<&str>,
    ) -> Option<(
        String,
        Vec<String>,
        Vec<String>,
        Vec<String>,
        Option<String>,
    )> {
        // The queue as well as the transcript: an early message left its `@file(...)`
        // mentions for this row to plan.
        let mut texts = self.store.user_texts(id).ok()?;
        texts.extend(self.store.queued_texts(name).unwrap_or_default());
        let borrowed: Vec<&str> = texts.iter().map(String::as_str).collect();
        let rules = crate::meta::rules(machines, home).await;
        let roots = crate::meta::roots(machines, home, &borrowed).await;
        if rules.is_none() && roots.is_empty() {
            return None;
        }
        let rates = self.rates(model);
        // What a message asked for outright: the rules count, since nothing dragged them
        // in for the cache.
        let mut explicit: Vec<String> = roots.iter().map(|root| root.spelling.clone()).collect();
        if rules.is_some() {
            explicit.push(crate::meta::rules_spelling());
        }
        let wanted: Vec<crate::prefix_cache::Wanted> = roots
            .iter()
            .map(|root| crate::prefix_cache::Wanted {
                key: root.spelling.as_str(),
                text: root.content.as_str(),
                tokens: self.tokens_on(&model, &root.content),
            })
            .collect();
        let planned = crate::prefix_cache::plan(
            &self.store,
            owner,
            "prompt plan",
            &wanted,
            rates,
            self.plan_tolerance(),
        );
        // Ancestors nobody named ride in so the cached prefix stays whole; the two lists
        // partition `rooted`, kept as a record.
        let rooted: Vec<String> = planned.entries.iter().map(|(key, _)| key.clone()).collect();
        let cached: Vec<String> = rooted
            .iter()
            .filter(|key| !explicit.contains(key))
            .cloned()
            .collect();
        let body = planned
            .entries
            .iter()
            .map(|(spelling, text)| {
                format!(
                    "{spelling} ({})
{text}",
                    crate::meta::machine_note(spelling, &machines.in_space(None), remote)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        // The rules first, out of the plan's reach: nothing moves or hides them. Kept by
        // reference as a record.
        let body = match &rules {
            Some(text) => {
                let spelling = crate::meta::rules_spelling();
                let rules = format!(
                    "{spelling} ({})\n{text}",
                    crate::meta::machine_note(&spelling, &machines.session, remote)
                );
                if body.is_empty() {
                    rules
                } else {
                    format!("{rules}\n\n{body}")
                }
            }
            None => body,
        };
        let prompts = crate::prompts::shared();
        let mut plan = prompts.render("reference_index", &[("entries", &body)]);
        if !planned.revisions.is_empty() {
            let changes = planned
                .revisions
                .iter()
                .map(|(spelling, patch)| format!("{spelling}\n{patch}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            plan.push_str("\n\n");
            plan.push_str(&prompts.render(crate::prompts::REVISION, &[("revisions", &changes)]));
        }
        Some((plan, rooted, explicit, cached, rules))
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
            tier,
            moderation,
        } = *attempt;
        let tuning = session.tuning();
        let current = self.config.now();
        let compact_mode = tuning
            .live_compact_mode
            .unwrap_or(current.agent.live_compact_mode);
        if compact_mode == crate::live_compact::CompactMode::Mask && !llm.supports_memory_mask() {
            anyhow::bail!(
                "memory mask mode requires endpoint api {:?}; use cut mode with this provider",
                crate::llm::MEMORY_MASK_API
            );
        }
        if compact_mode == crate::live_compact::CompactMode::Mask {
            let token_limit = self.memory_token_limit(llm, &tuning);
            let enforced = self.store.enforce_limit(id, token_limit, "request")?;
            if !enforced.auto_masked.is_empty() {
                self.log_event(
                    Some(id),
                    "memory_limit",
                    &json!({
                        "cause": "request",
                        "auto_masked": enforced.auto_masked,
                        "view_generation": enforced.view_generation,
                    }),
                );
            }
        }
        let generation = self.store.context_generation(id)?;
        let phase_started = std::time::Instant::now();
        // Read beside the stored prompt; a request assembles no part of its head.
        let tools = self.tool_set_in_force(session, id, llm.model(), llm)?;
        let space = session.space();
        let machines = crate::meta::Machines::of(&space);
        let prompt = self
            .prompt_in_force(session, id, llm.model(), cwd, &machines)
            .await?;
        phases.prompt_ms = phase_started.elapsed().as_millis() as u64;
        // A request cannot answer the model's own half step; fill the boundary first.
        self.fill_resume_mark(id, &name);
        // Pinned here, so every retry of this request replays the window that was
        // current when it started.
        let phase_started = std::time::Instant::now();
        let source = self.store.request_source(id, compact_mode)?;
        phases.source_ms = phase_started.elapsed().as_millis() as u64;
        // Where a failed attempt's rows end: the retry inside `llm.stream` sends this
        // window. See `docs/notes/server-agent.md`.
        let window = source.upto_seq;
        // One number for "how long the model took": the whole stream, retries included.
        let requested = std::time::Instant::now();
        // ttft and tps are the stream's own: a clock read here would count a tool as
        // generation.
        phases.llm_started = Some(requested);
        // One tier for the whole turn, settled before its first request.
        let tier = self.cache_tier(llm, tier);
        // One placement per head: the tool row's, or the info row's on an older head.
        let (section, before_card) = match prompt.parameters.as_deref() {
            Some(text) => (None, Some(text)),
            None => (tools.parameters.as_deref(), None),
        };
        let mut rx = llm.stream(
            &prompt.instructions,
            prompt.plan.as_deref(),
            section,
            before_card,
            prompt.tail.as_deref(),
            &source,
            &tools.tools,
            effort,
            tier.as_deref(),
            cancel.clone(),
        );

        // Which call each streamed item is, so its chunks are addressed by call id.
        let mut calls: std::collections::HashMap<String, String> = Default::default();
        // The parts already handed out as blocks: a refusal mid-item must not take the
        // next answer's text.
        let mut began: std::collections::HashSet<(String, ResponsePart)> = Default::default();
        // The items whose content the reader has already seen, so the failure line
        // can say that fragment is being withdrawn.
        let mut said: std::collections::HashSet<String> = Default::default();
        // Chunks that no completion event will store. See `docs/notes/server-agent.md`.
        // Two buffers: one per field.
        let mut streamed: std::collections::HashMap<String, (ResponsePart, String, String)> =
            Default::default();
        // When each draft was last written, so a fast stream is not one write per chunk.
        let mut drafted: std::collections::HashMap<String, std::time::Instant> = Default::default();
        let mut ran_tools = false;
        // An uncontinued call ends the turn, but only after every call has run.
        let mut ends_turn = false;
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
                    field,
                } => {
                    let call_id = calls.get(&item_id).cloned();
                    began.insert((item_id.clone(), part));
                    said.insert(item_id.clone());
                    let whole = streamed
                        .entry(item_id.clone())
                        .or_insert_with(|| (part, String::new(), String::new()));
                    match field {
                        Some(crate::llm::Field::SummaryText) => whole.2.push_str(&text),
                        Some(_) => whole.1.push_str(&text),
                        None => {}
                    }
                    // The draft is what survives a process that never finishes this
                    // item: the reader has already seen these chunks.
                    let due = drafted
                        .get(&item_id)
                        .is_none_or(|at| at.elapsed() >= DRAFT_FLUSH);
                    if due && text_worth_keeping(part) {
                        drafted.insert(item_id.clone(), std::time::Instant::now());
                        let _ = self.store.save_draft(
                            id,
                            &item_id,
                            turn,
                            part_name(part),
                            &whole.1,
                            &whole.2,
                        );
                    }
                    // The text and the chunk that carries it are one step: a
                    // snapshot read afterwards sees both, never one of them.
                    self.bus.step(|_| {
                        session.open_push(&item_id, turn, part, call_id.clone(), &text);
                        (
                            (),
                            Some(Event::Response(ResponseEvent {
                                session: name.to_string(),
                                turn,
                                step: u64::from(step),
                                part,
                                text: text.clone(),
                                done: false,
                                item_id: item_id.clone(),
                                call_id,
                            })),
                        )
                    });
                }
                LlmEvent::PartDone { part, item_id } => {
                    let call_id = calls.get(&item_id).cloned();
                    began.insert((item_id.clone(), part));
                    self.bus.publish(Event::Response(ResponseEvent {
                        session: name.to_string(),
                        turn,
                        step: u64::from(step),
                        part,
                        text: String::new(),
                        done: true,
                        item_id: item_id.clone(),
                        call_id,
                    }));
                }
                LlmEvent::Reasoning {
                    item_id,
                    item,
                    summary,
                } => {
                    // The item, not a summary of it; see `docs/notes/server-agent.md`.
                    let payload = json!({"item": item, "summary": summary, "item_id": item_id,
                                         "turn": turn, "step": step});
                    said.insert(item_id.clone());
                    streamed.remove(&item_id);
                    self.settle_item(session, &item_id);
                    self.announce_row(
                        name,
                        self.store.append_message(id, Kind::Reasoning, &payload),
                    );
                }
                // The provider executed this one; keeping the item verbatim is
                // what lets the next request carry the same transcript.
                LlmEvent::BuiltinCall {
                    item_id,
                    item,
                    replayable,
                } => {
                    said.insert(item_id.clone());
                    streamed.remove(&item_id);
                    self.settle_item(session, &item_id);
                    self.announce_row(
                        name,
                        self.store.append_message(
                            id,
                            Kind::BuiltinCall,
                            &json!({"item": item, "replayable": replayable,
                                    "turn": turn, "step": step}),
                        ),
                    );
                }
                LlmEvent::Citation { title, url, .. } => {
                    // One line per source: a search without its sources is unverifiable.
                    let text = format!("cited {title:?} — {url}");
                    self.bus.publish(Event::Response(ResponseEvent {
                        session: name.to_string(),
                        turn,
                        step: u64::from(step),
                        part: ResponsePart::Activity,
                        text: text.clone(),
                        done: true,
                        item_id: String::new(),
                        call_id: None,
                    }));
                    // Operator-only: the model keeps its text, not the annotation.
                    self.announce_row(
                        name,
                        self.store
                            .append_message(id, Kind::Notice, &json!({"text": text})),
                    );
                }
                LlmEvent::Output { item_id, text } => {
                    streamed.remove(&item_id);
                    self.settle_item(session, &item_id);
                    if !text.trim().is_empty() {
                        self.announce_row(
                            name,
                            self.store.append_message(
                                id,
                                Kind::Text,
                                &json!({"role":"assistant","text":text,
                                        "turn": turn, "step": step}),
                            ),
                        );
                    }
                }
                // Run it here, not after the stream: the call in flight is then
                // always the last row.
                LlmEvent::ToolCall {
                    item_id,
                    call_id,
                    name: tool,
                    arguments,
                } => {
                    // Its own text is the arguments, which this event stores.
                    streamed.remove(&item_id);
                    self.settle_item(session, &item_id);
                    // One start for the row, the event and the timing.
                    let started_at = crate::store::now_millis();
                    // Before it runs: this id is what the result carries, which
                    // is how a front-end holds a call and its output as one item.
                    self.bus.publish(Event::ToolCall(protocol::ToolCallEvent {
                        session: name.to_string(),
                        call_id: call_id.clone(),
                        head: call_head(&tool, &arguments),
                        started_at,
                    }));
                    self.announce_row(
                        name,
                        self.store.append_message(
                            id,
                            Kind::ToolCall,
                            &json!({"call_id": call_id, "name": tool, "arguments": arguments,
                                    "turn": turn, "step": step, "started_at": started_at}),
                        ),
                    );
                    let _ = self
                        .store
                        .start_tool_call(id, &call_id, &tool, &arguments, started_at);
                    match self
                        .run_one_call(
                            session, id, name, turn, step, cancel, llm, call_id, tool, arguments,
                            started_at,
                        )
                        .await
                    {
                        CallEnd::Stopped => return Ok(StepResult::Stopped),
                        CallEnd::EndsTurn => ends_turn = true,
                        CallEnd::Ran => {}
                    }
                    ran_tools = true;
                }
                LlmEvent::Attempt { probe } => {
                    let mut payload = probe.to_json();
                    payload["step"] = json!(step);
                    payload["turn"] = json!(turn);
                    self.log_event(Some(id), "llm_attempt", &payload);
                }
                LlmEvent::Completed {
                    usage,
                    window: stream,
                } => {
                    saw_completion = true;
                    // The plan it carried is warm again -- and only that one. The
                    // owner is the directory the prompt row states.
                    crate::prefix_cache::confirm(
                        &self.store,
                        &prompt.cwd,
                        "prompt plan",
                        &prompt.rooted,
                    );
                    // Outside the guard: a provider with no counts still took the time.
                    let ttft_ms = stream.first_delta_ms;
                    let tps = stream.tps(usage.as_ref().and_then(|u| u.output_tokens));
                    let stream_ms = stream
                        .first_delta_ms
                        .map(|first| stream.end_ms.saturating_sub(first));
                    if let Some(u) = &usage {
                        let cached = u.cached_tokens();
                        // Read before this answer is stored: the last input is
                        // the largest prefix this one could hit.
                        let expected = self.store.measured_tape_tokens(id).ok();
                        let gap_ms =
                            self.store.last_usage_at(id).ok().flatten().map(|at| {
                                crate::store::now_millis().saturating_sub(at).max(0) as u64
                            });
                        // The share the title draws: what of *this* request was served
                        // from cache. `expected` is the prefix it could have hit.
                        let hit_ratio = match (cached, u.input_tokens) {
                            (Some(hit), Some(size)) if size > 0 => Some(hit as f64 / size as f64),
                            _ => None,
                        };
                        let cache_write = u
                            .input_tokens_details
                            .as_ref()
                            .and_then(|d| d.cache_write_tokens);
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
                                "stream_ms": stream_ms,
                                "input": u.input_tokens,
                                "unmasked_input": u.gongga_unmasked_input_tokens,
                                "cached": cached,
                                "uncached": u.uncached_tokens(),
                                "expected": expected,
                                "hit_ratio": hit_ratio,
                                "gap_ms": gap_ms,
                                "cache_tier": tier.as_deref(),
                                "cache_write": cache_write,
                                "output": u.output_tokens,
                                "total": u.total_tokens,
                                "ttft_ms": ttft_ms,
                                "tps": tps,
                            }),
                        );
                    }
                    // The provider's own counts, never computed: a request that
                    // reported none keeps the last one it did.
                    if compact_mode == crate::live_compact::CompactMode::Mask
                        && let Some(measurement) = usage.as_ref()
                    {
                        if measurement.gongga_unmasked_input_tokens.is_none() {
                            anyhow::bail!(
                                "memory-mask provider omitted gongga_unmasked_input_tokens"
                            );
                        }
                        let masked = source
                            .memory
                            .as_ref()
                            .map(|view| view.masked.len())
                            .unwrap_or(0);
                        if measurement.gongga_memory_tokens.len() != masked {
                            anyhow::bail!(
                                "memory-mask provider reported {} block measurements for {masked} masked blocks",
                                measurement.gongga_memory_tokens.len()
                            );
                        }
                        self.store.note_memory_tokens(
                            id,
                            &measurement.gongga_memory_tokens,
                            measurement.gongga_unmasked_input_tokens.unwrap_or(0),
                        )?;
                    }
                    let context = usage
                        .as_ref()
                        .and_then(|u| {
                            if compact_mode == crate::live_compact::CompactMode::Mask {
                                u.gongga_unmasked_input_tokens
                            } else {
                                u.input_tokens
                            }
                        })
                        .or_else(|| self.store.context_tokens(id).ok())
                        .unwrap_or(0);
                    // A share of one request's context says nothing about the next
                    // one's, so an unreported count is not carried over.
                    let cached = usage.as_ref().and_then(|u| u.cached_tokens());
                    let measured = protocol::Usage {
                        context_tokens: context,
                        tape_tokens: usage.as_ref().and_then(|u| u.input_tokens),
                        cached_tokens: cached,
                        tps,
                        ttft_ms,
                    };
                    let _ = self
                        .store
                        .set_usage_for_generation(id, generation, measured);
                    if let Some(input_tokens) = usage.as_ref().and_then(|u| {
                        if compact_mode == crate::live_compact::CompactMode::Mask {
                            u.gongga_unmasked_input_tokens
                        } else {
                            u.input_tokens
                        }
                    }) {
                        if let Err(e) =
                            self.store
                                .set_live_compact_baseline(id, generation, input_tokens)
                        {
                            tracing::warn!(session = %name, error = %e, "could not set live compact baseline");
                        }
                    }
                    // SPEC: the row updates at every step's end. Off the turn's
                    // path, because reading an allowance is a network call.
                    crate::quota::publish(
                        self.bus.clone(),
                        self.store.clone(),
                        attempt.llm.provider().map(str::to_string),
                        self.config.now().quota_interval,
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
                    // Absorb a misfire; the fourth in one turn is a provider that keeps refusing.
                    if crate::llm::moderation_refusal(&reason) {
                        let seen = moderation.fetch_add(1, Ordering::Relaxed) + 1;
                        if seen > MODERATION_PER_TURN {
                            return Err(anyhow::Error::new(ProviderFailure(format!(
                                "the provider's content inspection refused {seen} of this turn's \
                                 requests; the turn stopped so the monitor sees a provider that \
                                 keeps refusing"
                            ))));
                        }
                    }
                    self.bus.publish(Event::Log {
                        level: LogLevel::Warn,
                        message: retry_warning(attempt, max, delay, &reason),
                        session: Some(name.to_string()),
                    });
                }
                // SPEC: nothing a cancel interrupts is lost. The chunks were
                // already handed to the reader, so the store gets them too.
                LlmEvent::Cancelled => {
                    self.keep_streamed(session, id, &name, turn, &mut streamed, &calls);
                    return Ok(StepResult::Stopped);
                }
            }
        }

        // Nothing follows a failure: `llm.stream` retried what another attempt could fix.
        if let Some(message) = failure {
            for (item_id, part) in began {
                let call_id = calls.get(&item_id).cloned();
                self.bus.publish(Event::Response(ResponseEvent {
                    session: name.to_string(),
                    turn,
                    step: u64::from(step),
                    part,
                    text: String::new(),
                    done: true,
                    item_id: item_id.clone(),
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
            // One line for the operator, in the retries' voice; taken-for-an-answer text
            // is never resumed.
            self.bus.publish(Event::Log {
                level: LogLevel::Warn,
                message: failed_warning(&name, &message, &said, discarded),
                session: None,
            });
            return Err(anyhow::Error::new(ProviderFailure(message)));
        }
        if !saw_completion {
            // The provider opened a stream and never closed it: the request, not the
            // harness, ended this step.
            return Err(anyhow::Error::new(ProviderFailure(
                "model stream ended without completing".to_string(),
            )));
        }
        if cancel.is_cancelled() {
            self.keep_streamed(session, id, &name, turn, &mut streamed, &calls);
            return Ok(StepResult::Stopped);
        }
        if !ran_tools {
            return Ok(StepResult::Finished);
        }
        if ends_turn {
            // `live_compact(continue = false)`: context written, no request after it.
            return Ok(StepResult::Finished);
        }
        tracing::debug!(session = %name, step, "step ran its calls as they arrived");
        Ok(StepResult::RanTools)
    }

    /// Turns the `@image(...)` references in a tool's output into attachments.
    ///
    /// # Invariant
    ///
    /// A reference that names no picture -- nothing readable there, or bytes that are
    /// not one -- is left as text, so a command printing one cannot fail the turn; a
    /// model that cannot take images is told so once, in the output, rather than having
    /// them silently dropped.
    async fn attach_references(
        &self,
        ctx: &ToolContext,
        printed_on: crate::session::Machine,
        output: &str,
    ) -> Vec<crate::image::ImageRef> {
        let mut out = Vec::new();
        // Read where printed, unless `remote://` names another. docs/notes/server-meta.md
        let printed_space = ctx.space_on(printed_on);
        let machines = crate::meta::Machines::of(&printed_space);
        let named: Vec<(String, Option<String>)> = crate::meta::mentions(output)
            .into_iter()
            .filter(|m| m.kind == protocol::Reference::Image)
            .map(|m| (m.value, m.remote))
            .collect();
        for (given, stated) in named {
            // Read on the machine that printed it: a `bash` picture here, a `remote_bash`
            // one there.
            let space = machines.in_space(stated.as_deref());
            let dir = match stated.as_deref() {
                None | Some("none") => ctx.dir(printed_on),
                Some(_) => ctx.session_dir(),
            };
            let path = space.spell(&dir, &given);
            let bytes = match space.read_bytes(&path).await {
                Some(b) => b,
                // Not a picture this harness can read: leave the text alone.
                None => continue,
            };
            let Some(media) = crate::image::media_type(&bytes) else {
                continue;
            };
            if !ctx.vision.accepts_image {
                self.bus.publish(Event::Log {
                    level: LogLevel::Warn,
                    message: format!(
                        "{}: {given} was referenced but not attached -- {}",
                        ctx.session, ctx.vision.refusal
                    ),
                    session: None,
                });
                continue;
            }
            let mut image = crate::image::ImageRef::new(path, media);
            // Stored too: nothing is uploaded here, the provider's file is resolved per
            // request.
            image.copy =
                crate::image::pictures::remember(&self.store, &image.path, media, &bytes).ok();
            out.push(image);
        }
        out
    }

    /// Runs the one call the model just asked for, and says what the step does next.
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
        started_at: i64,
    ) -> CallEnd {
        let started = std::time::Instant::now();
        if cancel.is_cancelled() {
            // Its row exists and a provider wants it answered; a row the body would
            // otherwise invent is written once instead.
            self.answer_interrupted(id, name, &call_id, &tool, turn, step);
            return CallEnd::Stopped;
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

        let machine = session.owner();
        let ctx = ToolContext {
            session: name.to_string(),
            call_id: call_id.clone(),
            tool: tool.clone(),
            channels: self.channels.clone(),
            home: self.home.clone(),
            output_dir: self.output_dir.clone(),
            workspace: session.workspace(),
            space: session.space(),
            machine,
            remote: session.remote_name(),
            events: Some(tx),
            cancel: cancel.clone(),
            store: self.store.clone(),
            cwd: session.cwd(),
            env: session.env(),
            vision: llm.vision(crate::prompts::shared()),
            inbox: session.inbox_signal(),
            stop: session.stop_signal(),
            closing: session.closing_signal(),
            // The threshold the config states now, not the one this process started with.
            disk_low: self.config.now().disk_low,
            explore: session.explore(),
        };
        let run_started = std::time::Instant::now();
        tracing::info!(session = %name, tool = %tool, "tool start");
        let printed_on = self.tools().machine_of(&tool, &arguments, &ctx);
        // Held for the whole run: the table a call started in is the one it finishes in.
        let table = self.tools();
        let outcome = table.run(&tool, arguments, &ctx).await;
        let run_ms = run_started.elapsed().as_millis() as u64;
        let attach_started = std::time::Instant::now();
        let scanned = self
            .attach_references(&ctx, printed_on, &outcome.output)
            .await;
        let attach_ms = attach_started.elapsed().as_millis() as u64;
        // The call layer capped this answer: nothing is done to it again, since a second
        // cap is a second answer.
        let capped = outcome.output.clone();
        // A tool that streamed nothing owes its output to the final chunk.
        let mut tail = match sent.load(Ordering::Relaxed) {
            0 => capped.clone(),
            _ => String::new(),
        };
        // What the tool said: the duration rides the head, not the text. `server-agent.md`.
        let mut output = capped;
        drop(ctx);
        // A shell on an endpoint starts in a directory only that endpoint knows.
        session.adopt_reported_cwd();
        let _ = forwarder.await;
        let mut status = if outcome.ok { "ok" } else { "error" };
        // A command that printed `@image: <path>` names a picture: it becomes an
        // attachment, uploaded once per file.
        let images: Vec<_> = scanned
            .iter()
            .map(crate::image::ImageRef::to_json)
            .collect();
        let content = json!({"call_id": call_id, "output": output, "images": images,
                    "turn": turn, "step": step, "ok": outcome.ok, "elapsed_ms": run_ms});
        // A command may end the turn or store its own result row; a stored row is
        // read back, never appended twice.
        let ends_turn = outcome.end_turn;
        match outcome.result_seq {
            Some(seq) => match self.store.stored_tool_result(id, seq) {
                Ok((stored, stored_ok)) => {
                    output = stored;
                    status = if stored_ok { "ok" } else { "error" };
                    tail = output.clone();
                }
                Err(e) => {
                    tracing::warn!(session = %name, seq, error = %e, "a command's stored result row could not be read");
                }
            },
            None => {
                self.announce_row(
                    name,
                    self.store.append_message(id, Kind::ToolResult, &content),
                );
            }
        }
        let tool_ok = status == "ok";
        tracing::info!(
            session = %name,
            tool = %tool,
            ok = tool_ok,
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
            head: Some(result_head(tool_ok, run_ms)),
        }));
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
        if ends_turn {
            CallEnd::EndsTurn
        } else {
            CallEnd::Ran
        }
    }

    /// Answers a call that will never run, so no stored call is left unanswered.
    ///
    /// # Invariant
    ///
    /// Every `tool_call` row is followed by exactly one `tool_result` row for its
    /// id. A request used to invent the missing one while writing the body, which
    /// meant the same transcript could go out as two different byte sequences; the
    /// missing answer is a row now, written once -- and the window is told, because a
    /// block left open is a block the reader cannot tell from a running tool.
    fn answer_interrupted(
        &self,
        id: i64,
        name: &str,
        call_id: &str,
        tool: &str,
        turn: u64,
        step: u32,
    ) {
        let said = crate::store::interrupted_call();
        self.announce_row(
            name,
            self.store.append_message(
                id,
                Kind::ToolResult,
                &json!({
                    "call_id": call_id,
                    "output": said,
                    "images": [],
                    "turn": turn,
                    "step": step,
                    "ok": false,
                    "elapsed_ms": 0,
                }),
            ),
        );
        let _ = self.store.finish_tool_call(id, call_id, &said, "cancelled");
        self.bus.publish(Event::ToolAnswer(ToolAnswerEvent {
            session: name.to_string(),
            call_id: call_id.to_string(),
            tool: tool.to_string(),
            stream: ToolStream::Stdout,
            text: String::new(),
            done: true,
            head: Some(result_head(false, 0)),
        }));
    }

    /// Forgets the drafts of items whose finished rows were just written.
    ///
    /// # Invariant
    ///
    /// Called wherever a real row is appended, so a draft never outlives the row it
    /// became: a leftover draft would be a second copy of the same answer, appended at
    /// the next start under a `canceled` flag that is no longer true.
    fn settle_item(&self, session: &SessionHandle, item_id: &str) {
        let _ = self.store.drop_draft(session.id(), item_id);
        // The row says it now, so the part stops being one a refill sends.
        session.open_done(item_id);
    }

    /// Publishes the operator's own lines, then where the transcript now ends.
    ///
    /// # Invariant
    ///
    /// The lines come before the announcement: a window reads that as "this row is
    /// drawn", so a row announced first is one it never fetches. `Input` is the
    /// shape `commands::row_event` replays, so live and refilled agree.
    /// See `docs/notes/server-agent.md`.
    pub(crate) fn announce_operator_rows(&self, session: &str, rows: crate::store::OperatorRows) {
        for line in &rows.lines {
            // The harness's own words are its own block: the operator's echo is
            // drawn as theirs, and this as the harness talking.
            let part = match line.injected {
                true => ResponsePart::Inject,
                false => ResponsePart::Input,
            };
            self.bus.publish(Event::Response(ResponseEvent {
                session: session.to_string(),
                turn: protocol::REPLAY_TURN,
                step: 0,
                part,
                text: line.text.clone(),
                done: true,
                item_id: String::new(),
                call_id: None,
            }));
        }
        self.announce_row(session, Ok(rows.seq));
    }

    /// Publishes where a session's transcript now ends, after a row was written.
    ///
    /// # Invariant
    ///
    /// Nothing is announced for a row that was not written: a position that names
    /// no row is a position a client cannot check. See
    /// `docs/notes/protocol-message.md`.
    pub(crate) fn announce_row(&self, session: &str, written: anyhow::Result<i64>) {
        match written {
            Ok(seq) => {
                self.bus.publish(Event::Transcript {
                    session: session.to_string(),
                    seq,
                });
            }
            Err(e) => {
                tracing::warn!(session = %session, error = %e, "a row was not written");
            }
        }
    }

    /// Stores what a cancelled item had already said, and closes its block.
    ///
    /// # Invariant
    ///
    /// Only rows no completion event wrote, so a stored item is never restated; and
    /// what is stored is what a request may send. See `docs/notes/server-agent.md`.
    fn keep_streamed(
        &self,
        session: &SessionHandle,
        id: i64,
        name: &str,
        turn: u64,
        streamed: &mut std::collections::HashMap<String, (ResponsePart, String, String)>,
        calls: &std::collections::HashMap<String, String>,
    ) {
        for (item_id, (part, text, summary)) in streamed.drain() {
            if !text.trim().is_empty() || !summary.trim().is_empty() {
                let stored = match part {
                    ResponsePart::Output => Some((
                        Kind::Text,
                        json!({"role": "assistant", "text": text, "interrupted": true,
                               "turn": turn}),
                    )),
                    ResponsePart::Reasoning => {
                        crate::store::reasoning_item(&item_id, &text, &summary).map(|item| {
                            let shown = if text.trim().is_empty() {
                                summary
                            } else {
                                text
                            };
                            (
                                Kind::Reasoning,
                                json!({"item": item, "summary": shown,
                                       "item_id": item_id, "interrupted": true,
                                       "turn": turn}),
                            )
                        })
                    }
                    // A call's text is its arguments; an activity is progress;
                    // the operator's words are not the model's to store.
                    ResponsePart::ToolCall
                    | ResponsePart::Activity
                    | ResponsePart::Input
                    | ResponsePart::Inject => None,
                };
                if let Some((kind, payload)) = stored {
                    self.announce_row(name, self.store.append_message(id, kind, &payload));
                }
            }
            self.bus.publish(Event::Response(ResponseEvent {
                session: name.to_string(),
                turn,
                step: 0,
                part,
                text: String::new(),
                done: true,
                item_id: item_id.clone(),
                call_id: calls.get(&item_id).cloned(),
            }));
            self.settle_item(session, &item_id);
        }
    }

    /// Fills the boundary of a request that would otherwise end on the model's own words.
    ///
    /// # Invariant
    ///
    /// One row at most per request: what it appends is a user turn, so the check after it
    /// finds a tail to answer. A home with no text for the mark sends the request as it
    /// stands rather than an empty turn. See `docs/notes/server-agent.md`.
    fn fill_resume_mark(&self, id: i64, session: &str) {
        match self.store.answerable_tail(id) {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    session = %session, error = %e,
                    "cannot tell whether the request ends on the model's own words"
                );
                return;
            }
        }
        let text = crate::prompts::shared().render(crate::prompts::RESUME_MARK, &[]);
        if text.trim().is_empty() {
            tracing::warn!(
                session = %session,
                "the transcript stops on the model's own words, and this home has no resume mark to fill"
            );
            return;
        }
        match self.store.append_message(
            id,
            Kind::ThinkingMark,
            &json!({"text": crate::prompts::harness_wrap(text)}),
        ) {
            Ok(seq) => {
                self.announce_row(session, Ok(seq));
                tracing::info!(
                    session = %session,
                    "filled a resume boundary: the request would have ended on the model's own words"
                );
            }
            Err(e) => {
                tracing::warn!(session = %session, error = %e, "cannot append the resume mark")
            }
        }
    }

    /// Appends the row that answers a refusal to continue a step written without thinking.
    ///
    /// # Invariant
    ///
    /// Called only after the provider refused, so a session the provider accepts never
    /// carries one; see `docs/notes/server-agent.md`.
    fn append_thinking_mark(&self, id: i64, session: &str) {
        match self.store.append_message(
            id,
            Kind::ThinkingMark,
            &json!({"text": crate::prompts::harness_wrap(crate::prompts::shared().render(crate::prompts::THINKING_MARK, &[]))}),
        ) {
            Ok(seq) => {
                self.announce_row(session, Ok(seq));
                tracing::info!(
                    session = %session,
                    "appended a turn boundary: the provider refused the transcript without one"
                )
            }
            Err(e) => {
                tracing::warn!(session = %session, error = %e, "cannot append the thinking marker")
            }
        }
    }

    /// Reports a failure, and leaves the reason where both of its readers look.
    ///
    /// # Invariant
    ///
    /// One row, of kind `error`: the sentence is context the next request carries and
    /// the line a window draws red. A notice would be neither (nothing is sent from
    /// one), and a line no event drew is a row no window ever shows.
    fn error(&self, id: i64, session: &str, what: &str, e: anyhow::Error) {
        tracing::error!(session = %session, "failed to {what}: {e}");
        self.write_error(id, session, &format!("{session}: failed to {what}: {e}"));
    }

    /// Writes the failure and then draws it, in that order.
    ///
    /// # Invariant
    ///
    /// A window reads the stamp as "I have drawn that row", so the event that draws it
    /// goes first. A row that could not be written leaves the log's own line and no
    /// claim about a window. See `docs/notes/server-agent.md`.
    fn write_error(&self, id: i64, session: &str, said: &str) {
        match self
            .store
            .append_message(id, Kind::Error, &json!({"text": said}))
        {
            Ok(seq) => {
                self.bus.publish(Event::Log {
                    level: LogLevel::Error,
                    message: said.to_string(),
                    session: Some(session.to_string()),
                });
                self.bus.publish(Event::Transcript {
                    session: session.to_string(),
                    seq,
                });
            }
            Err(e) => tracing::warn!(session = %session, error = %e, "the failure was not written"),
        }
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

/// What one call leaves the step to do.
enum CallEnd {
    /// Nothing special: the step goes on to its next call, or to its next request.
    Ran,
    /// The turn is over once every call of this step has been answered.
    EndsTurn,
    /// Cancellation: the step stops at once.
    Stopped,
}

/// One model request: which round of the turn it is, and how to make it.
#[derive(Clone, Copy)]
struct Attempt<'a> {
    number: u32,
    llm: &'a LlmClient,
    /// The provider's inspection refusals this turn has already absorbed.
    moderation: &'a std::sync::atomic::AtomicU32,
    /// The session's static tier, so every step sends the same one.
    tier: Option<&'a str>,
    /// How hard the model should think, when the session asked for a level.
    effort: Option<&'a str>,
    /// Where the session's shell stands, which the row records so a move is seen.
    cwd: &'a std::path::Path,
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

/// The line for a request that is about to be tried again.
///
/// # Invariant
///
/// `max == 0` is "no ceiling", not "zero allowed": printing `1/0` read like the
/// opposite of what it meant. The session is not in the words: the event carries it,
/// and the line is drawn in that session's own window.
fn retry_warning(attempt: u32, max: u32, delay: std::time::Duration, reason: &str) -> String {
    let of = if max == 0 {
        "until cancelled".to_string()
    } else {
        format!("of {max}")
    };
    format!("model request failed ({reason}); retry {attempt} {of} in {delay:?}")
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
/// what is possible. See the `handoff` skill.
fn context_notice(
    prompts: &crate::prompts::Prompts,
    session: &str,
    tokens: u64,
    budget: u64,
    window: Option<u64>,
) -> String {
    let total = window.unwrap_or(budget);
    crate::prompts::harness_wrap(prompts.render(
        "context_notice",
        &[
            ("session", session),
            ("tokens", &tokens.to_string()),
            ("budget", &budget.to_string()),
            ("window", &total.to_string()),
        ],
    ))
}

/// What an archive is told: the measurement, and nothing it could act on.
///
/// # Invariant
///
/// The same two numbers the agent's notice carries -- where the context is and how
/// much there is altogether -- and no instruction beside them: there is no tool an
/// archive could run to make itself smaller. `budget` stands in for a window the
/// roster does not state, exactly as it does for the agent's notice.
fn context_measure(
    prompts: &crate::prompts::Prompts,
    session: &str,
    tokens: u64,
    budget: u64,
    window: Option<u64>,
) -> String {
    let total = window.unwrap_or(budget);
    crate::prompts::harness_wrap(prompts.render(
        crate::prompts::CONTEXT_MEASURE,
        &[
            ("session", session),
            ("tokens", &tokens.to_string()),
            ("window", &total.to_string()),
        ],
    ))
}

/// What the server asks for when the interval elapses.
///
/// # Invariant
///
/// It asks for what a later turn cannot recover and nothing else: the round the
/// server keeps verbatim is not restated, a file is cited rather than pasted, and
/// the answer stays short -- every word here is paid again on every later request,
/// because a summary is never a cache hit. See `docs/notes/server-live-compact.md`.
fn live_compact_notice() -> String {
    crate::prompts::harness_wrap(
        crate::prompts::shared().render(crate::prompts::LIVE_COMPACT_NOTICE, &[]),
    )
}

/// The mask-mode ask shares the interval but commits a view, never a transcript cut.
fn memory_mask_notice() -> String {
    crate::prompts::harness_wrap(
        crate::prompts::shared().render(crate::prompts::MEMORY_MASK_NOTICE, &[]),
    )
}

/// The notice threshold in input tokens, or 0 for "off".
///
/// # Invariant
///
/// `fallback` is what a session that names no percentage gets, and it comes from the
/// config: the sentinel is not spelled here, so a home that changed it changes this.
fn notice_budget(configured: u64, fallback: u8, context: Option<u64>, percent: Option<u8>) -> u64 {
    if configured == 0 {
        return 0;
    }
    let Some(window) = context.filter(|w| *w > 0) else {
        return configured;
    };
    providers::tokens_for_percent(window, percent.unwrap_or(fallback))
}

/// This machine's own cached-prefix lifetime, in millis; `None` leaves the provider's.
fn cache_ttl_ms(config: &crate::config::Config) -> Option<u64> {
    (config.agent.cache_ttl_secs > 0).then(|| config.agent.cache_ttl_secs.saturating_mul(1_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_config_turns_the_notice_off_whatever_the_window_is() {
        assert_eq!(notice_budget(0, 70, Some(1_000_000), Some(50)), 0);
        assert_eq!(notice_budget(0, 70, Some(1_000_000), None), 0);
        assert_eq!(notice_budget(0, 70, None, None), 0);
    }

    /// A known window decides the threshold, so a bigger model needs no retuning.
    #[test]
    fn a_known_window_beats_the_configured_number() {
        assert_eq!(notice_budget(40_000, 70, Some(1_000_000), None), 700_000);
        assert_eq!(notice_budget(40_000, 70, Some(200_000), Some(50)), 100_000);
        // No window in the roster: the configured number is all there is.
        assert_eq!(notice_budget(40_000, 70, None, Some(50)), 40_000);
        assert_eq!(notice_budget(40_000, 70, None, None), 40_000);
    }

    /// The share a session names nothing for is the config's, not a constant here.
    #[test]
    fn the_fallback_share_is_whatever_the_config_says() {
        assert_eq!(notice_budget(40_000, 70, Some(1_000_000), None), 700_000);
        assert_eq!(notice_budget(40_000, 50, Some(1_000_000), None), 500_000);
        // A session that named a percentage keeps it whatever the fallback is.
        assert_eq!(
            notice_budget(40_000, 50, Some(1_000_000), Some(30)),
            300_000
        );
    }

    /// Every value the template can name has to arrive: the words are the
    /// config's, the numbers are this function's.
    #[test]
    fn the_context_notice_says_what_to_do() {
        let notice = context_notice(&notice_prompts(), "main", 700_000, 700_000, Some(1_000_000));
        assert!(
            notice.contains("700000 input tokens of 1000000 total"),
            "{notice}"
        );
        assert!(notice.contains("for `main`"), "{notice}");
        assert!(notice.contains("transcript handoff"), "{notice}");
        assert!(notice.contains("Hand off soon"), "{notice}");
        assert!(
            notice.starts_with("<harness>") && notice.ends_with("</harness>"),
            "{notice}"
        );
        assert!(
            !notice.contains("{{"),
            "a placeholder nobody filled: {notice}"
        );
    }

    /// One directory, two spellings a shell may report: one string in the prompt.
    ///
    /// # Invariant
    ///
    /// The tail is cached prose, so a re-spelling must not rewrite it.
    #[test]
    fn a_directory_is_stated_one_way() {
        let dir = std::env::temp_dir().join(format!("gongga-stated-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory to spell");
        let forward = dir.display().to_string().replace('\\', "/");
        let slashed = std::path::PathBuf::from(&forward);
        assert_eq!(
            crate::session::stated(&dir),
            crate::session::stated(&slashed),
            "{forward} and {} name one directory",
            dir.display()
        );
        assert!(
            !crate::session::stated(&dir).starts_with(r"\\?\"),
            "the canonical prefix is not prose"
        );
        let _ = std::fs::remove_dir(&dir);
    }

    /// The prompt row's own facts are stamped once, so a later read cannot move them.
    ///
    /// # Invariant
    ///
    /// What a request has sent never changes: the row is appended, and the start card
    /// is carried from the store rather than re-rendered from the clock.
    #[test]
    fn the_start_card_is_stamped_and_then_read_back() {
        let store =
            crate::store::Store::open(std::path::Path::new(":memory:")).expect("a throwaway store");
        let id = store
            .create_session_on("s", "m", ".", None, None)
            .expect("a session");
        let first = crate::store::Prompt {
            instructions: "prose".to_string(),
            plan: None,
            agents_md: None,
            tail: Some("Session `s`, 2026-11-01 14:00 UTC".to_string()),
            parameters: Some("every call may carry `description`".to_string()),
            rooted: Vec::new(),
            plan_explicit: Vec::new(),
            plan_cached: Vec::new(),
            cwd: ".".to_string(),
            remote: None,
            model: Some("m".to_string()),
        };
        store.append_prompt(id, &first).expect("one row");
        // The head is five rows: instructions/1, tools/2, agents.md/3, plan/4, info/5.
        let read = store.prompt(id).expect("a readable row").expect("one row");
        assert_eq!(read, first, "the five rows rebuild the same prompt");
        // The first transcript row follows the five head slots.
        let seq = store
            .append_message(
                id,
                crate::store::Kind::Text,
                &serde_json::json!({"role":"user","text":"hello"}),
            )
            .expect("a message");
        assert_eq!(seq, 6, "ordinary messages start after the five head rows");
    }

    #[test]
    fn a_five_row_head_rebuilds_every_wire_field() {
        let store =
            crate::store::Store::open(std::path::Path::new(":memory:")).expect("a throwaway store");
        let id = store
            .create_session_on("s", "m", ".", None, None)
            .expect("a session");
        let prompt = crate::store::Prompt {
            instructions: "the system prose".to_string(),
            plan: Some("the expanded plan".to_string()),
            agents_md: Some("the standing rules".to_string()),
            tail: Some("the card".to_string()),
            parameters: Some("the shared args".to_string()),
            rooted: vec!["@file(a.md)".to_string()],
            plan_explicit: vec!["@file(a.md)".to_string()],
            plan_cached: vec!["@skill(b)".to_string()],
            cwd: "/here".to_string(),
            remote: Some("peer".to_string()),
            model: Some("m".to_string()),
        };
        store.append_prompt(id, &prompt).expect("five rows");
        let read = store.prompt(id).expect("readable").expect("one prompt");
        // Every field the wire head is built from survives the round trip.
        assert_eq!(read, prompt);
    }

    #[test]
    fn the_five_head_rows_each_render_as_one_line() {
        let store =
            crate::store::Store::open(std::path::Path::new(":memory:")).expect("a throwaway store");
        let id = store
            .create_session_on("s", "m", ".", None, None)
            .expect("a session");
        let prompt = crate::store::Prompt {
            instructions: "the standing prose".to_string(),
            plan: Some("the expanded plan".to_string()),
            agents_md: Some("the standing rules".to_string()),
            tail: Some("the card".to_string()),
            parameters: Some("shared args".to_string()),
            rooted: vec![],
            plan_explicit: vec!["@file(a.md)".to_string()],
            plan_cached: vec!["@skill(old)".to_string()],
            cwd: "/home/session".to_string(),
            remote: None,
            model: Some("m".to_string()),
        };
        store.append_prompt(id, &prompt).expect("five rows");
        store
            .write_head_once(
                id,
                &crate::store::ToolSet {
                    tools: serde_json::json!([{"type":"function","name":"read_file"}]),
                    parameters: None,
                    model: None,
                    remote: None,
                    build: String::new(),
                    event: String::new(),
                },
            )
            .expect("the tool row");
        let window = store.messages_between(id, 1, 5).expect("the head window");
        let texts: Vec<&str> = window.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts.len(), 5, "five stored head rows render as five lines");
        // The plan names its references and why, and never carries their bodies.
        assert!(texts[0].starts_with("instructions: the standing prose"));
        assert!(texts[1].starts_with("tools: 1 tool(s) [read_file]"));
        assert!(texts[2].starts_with("agents.md: the standing rules"));
        assert!(texts[3].starts_with("plan: 2 reference(s)"));
        assert!(texts[3].contains("\n  required: @file(a.md)"));
        assert!(texts[3].contains("\n  cached: @skill(old)"));
        assert!(
            !texts[3].contains("the expanded plan"),
            "a body the provider caches is not a line the reader wants"
        );
        assert!(texts[4].starts_with("info: cwd=/home/session"));
    }

    #[test]
    fn a_tools_row_shows_the_names_and_the_whole_schema() {
        let display = serde_json::json!({
            "tools": [
                {"type": "function", "name": "read_file"},
                {"type": "deepseek_web_search"}
            ]
        })
        .to_string();
        let row = crate::store::MessageRow::parse(2, "tools", &display);
        // The names are the fold; the schema is the body an opened block draws.
        assert_eq!(
            row.text.lines().next(),
            Some("tools: 2 tool(s) [read_file, deepseek_web_search]")
        );
        let body = &row.text[row.text.find('\n').expect("a body below the names")..];
        let schema: serde_json::Value = serde_json::from_str(body).expect("the schema as written");
        assert_eq!(
            schema,
            serde_json::json!([
                {"type": "function", "name": "read_file"},
                {"type": "deepseek_web_search"}
            ])
        );
    }

    fn notice_prompts() -> crate::prompts::Prompts {
        crate::prompts::Prompts::of(&[(
            "context_notice",
            "Context notice for `{{session}}`: {{tokens}} input tokens of {{window}} total (handoff threshold {{budget}}). Hand off soon; first put the handoff materials in order. Run `<handoff>/transcript handoff --note <text>` for `{{session}}`.",
        )])
    }

    /// The warning names the cause: "failed or came back empty" is what made
    /// "why did the first attempt fail" unanswerable.
    #[test]
    fn a_retry_warning_names_why() {
        let said = retry_warning(2, 0, std::time::Duration::from_secs(2), "HTTP 500: boom");
        assert!(said.contains("HTTP 500: boom"), "{said}");
        assert!(said.contains("retry 2 until cancelled"), "{said}");
        assert!(said.contains("2s"), "{said}");
        // An explicit ceiling still reads as a ceiling, not as a fraction of 0.
        let said = retry_warning(1, 3, std::time::Duration::from_secs(1), "empty");
        assert!(said.contains("retry 1 of 3"), "{said}");
    }

    /// The notice names the two numbers it can know: the measured size and the budget.
    #[test]
    fn the_notice_names_the_size_and_the_budget() {
        let notice = context_notice(&notice_prompts(), "main", 10, 20, Some(100));
        assert!(notice.contains("10 input tokens of 100 total"), "{notice}");
        assert!(notice.contains("handoff threshold 20"), "{notice}");
        assert!(!notice.contains("{{"), "{notice}");
    }
    /// The row a call is drawn with keeps its shared arguments: a title is read, not taken.
    ///
    /// # Invariant
    ///
    /// Reading the title must not eat the other shared names -- a row that silently lost `max_output`
    /// would show a call that never stated it.
    #[test]
    fn the_row_keeps_the_shared_arguments_it_drew_a_title_from() {
        let value = serde_json::json!(
            {"description": "check the state", "max_output": 8000, "path": "a-file"}
        );
        let head = call_head("read", &value.to_string());
        assert_eq!(head.description, "check the state");
        let keys: Vec<&str> = head.fields.iter().map(|f| f.label.as_str()).collect();
        assert!(keys.contains(&"max_output"), "{keys:?}");
        assert!(keys.contains(&"path"), "{keys:?}");
    }
}
/// What a call is, and the words it is read with: the tool and its arguments.
///
/// # Invariant
///
/// Kind and text only -- never a glyph or a colour. This crate knows what a tool
/// is; how that looks belongs to the front-end, so restyling is a change to one
/// crate and nothing on the wire moves. See `docs/notes/protocol-message.md`.
pub(crate) fn call_head(tool: &str, arguments: &str) -> protocol::Head {
    let value: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    // The title comes from the list the call layer reads, never a second literal: the row
    // still carries `max_output`.
    let title = crate::tools::laws::Given::take(&mut value.clone()).description;
    protocol::Head::call(
        tool,
        title.as_deref().unwrap_or_default(),
        call_fields(tool, &value),
    )
}

/// A call's arguments, each marked with whether SPEC's `abstract` shows it.
///
/// # Invariant
///
/// The per-tool list is this crate's: it owns the tools, so it knows an `exec`
/// call is summarised by `{bin, wait}` and that
/// `command` is the detail's. The wire carries the verdict per field, so adding a
/// tool touches nothing a front-end reads. Every argument is sent once: what the
/// summary leaves out is here, so opening needs no request.
fn call_fields(tool: &str, value: &serde_json::Value) -> Vec<protocol::Field> {
    let summary = summary_keys(tool);
    // SPEC's `toolcall{tool_name, ...}`: the name rides on the head.
    let mut fields = Vec::new();
    // The detail first, so opening the item leads with what was run.
    if let Some(command) = value.get("command") {
        fields.push(protocol::Field {
            label: String::new(),
            value: command_text(command),
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
            if !owns_row(key) && !ordered.contains(key) {
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

/// A `command` argument as one line of text, whichever shape it came in.
///
/// # Invariant
///
/// A list of words is shown as the list it is, because a word holding a space
/// and two words are the same text once they are joined: the reader has to be
/// able to tell them apart.
fn command_text(command: &serde_json::Value) -> String {
    match command {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(words) => serde_json::to_string(words).unwrap_or_default(),
        other => other.to_string(),
    }
}

/// The arguments SPEC's `toolcall` summarises this tool by, in its order.
fn summary_keys(tool: &str) -> &'static [&'static str] {
    match tool {
        // SPEC's `exec(bin #abstraction, wait #abstraction)`: the program it
        // names, and how long it stays to hear it.
        "exec" => &["bin", "wait"],
        _ => &[],
    }
}

/// Whether an argument already has a row of its own, so it is not listed again.
///
/// # Invariant
///
/// `command` is the detail's first row, and `description` is the head's title; an
/// argument written twice is a row that says the same thing twice, and the second
/// copy would be the one the model's words were escaped into.
fn owns_row(key: &str) -> bool {
    matches!(key, "command" | "description")
}

/// The name the tool's own schema uses, where SPEC spells it otherwise.
fn tool_key(name: &str) -> &str {
    match name {
        "time_out" => "timeout_ms",
        other => other,
    }
}

/// The name SPEC gives an argument, where the tool's own schema spells it otherwise.
fn spec_name(key: &str) -> &str {
    match key {
        "timeout_ms" => "time_out",
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
    protocol::Head::new(kind, protocol::spent(run_ms))
}

/// How often a streaming item's draft is written: one write per item per interval.
const DRAFT_FLUSH: std::time::Duration = std::time::Duration::from_millis(250);

/// Whether a part's text is worth keeping before the item finishes: a call's text is
/// its arguments.
fn text_worth_keeping(part: ResponsePart) -> bool {
    matches!(part, ResponsePart::Output | ResponsePart::Reasoning)
}

/// The name a draft stores a part under, so `settle_drafts` can read it back.
fn part_name(part: ResponsePart) -> &'static str {
    match part {
        ResponsePart::Output => "output",
        ResponsePart::Reasoning => "reasoning",
        ResponsePart::ToolCall => "tool_call",
        ResponsePart::Activity => "activity",
        // Never streamed: a replayed message is a row, and a row is not drafted.
        ResponsePart::Input => "input",
        ResponsePart::Inject => "inject",
    }
}
