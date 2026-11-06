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
use crate::config::ModelConfig;
use crate::llm::{LlmClient, LlmEvent};
use crate::providers::{self};
use crate::session::SessionHandle;
use crate::store::Store;
use crate::tools::detached::DetachedRegistry;
use crate::tools::{ToolContext, ToolRegistry};

/// How much of a turn's answer a `turn_end` row carries.
///
/// # Invariant
///
/// The answer is a convenience, not the record: the transcript keeps all of it,
/// so a row that carries the head of a long answer loses nothing that cannot be
/// read again.
const ANSWER_LIMIT: usize = 32_000;

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
    /// This machine's own lifetime for a cached prefix, in millis; `None` leaves the
    /// provider's number in force.
    pub cache_ttl_ms: Option<u64>,
    /// The share of an indexed entry's lines that may change and still be patched.
    pub index_change_tolerance: f64,
    /// Compress once a request would carry more than this many input tokens.
    /// 0 disables it.
    pub compress_above_tokens: u64,
    /// Share of the window a session states no percentage for.
    pub context_limit_percent: u8,
    /// The home this call belongs to: what a worker needs to be started.
    pub home: PathBuf,
    /// Whether the agent is asked to write checkpoint summaries; the switch, not the
    /// threshold, is the opt-in.
    pub live_compact_enabled: bool,
    /// Input-token growth between agent-written checkpoints; the threshold the
    /// enabled feature measures against.
    pub live_compact_interval_tokens: u64,
    /// Where capped tool output is spilled; without it the tail is lost.
    pub output_dir: PathBuf,
    /// Detached jobs of every session, reached through `ToolContext`.
    pub detached: DetachedRegistry,
    /// Jobs running in this process, so a call can stop one by its id.
    pub jobs: crate::tools::tunnel::JobRegistry,
}

impl Agent {
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
        // No part outlives its turn: what is left is text whose row was never written.
        session.open_clear();
        // The turn's own words ride the row: a subscriber reads them there.
        let answer = self
            .store
            .last_answer(session.id())
            .unwrap_or(None)
            .map(|text| {
                if text.chars().count() > ANSWER_LIMIT {
                    let head: String = text.chars().take(ANSWER_LIMIT).collect();
                    format!("{head}\n… (the answer continues in the transcript)")
                } else {
                    text
                }
            });
        // The turn's outcome as a row, not only a line: a reader with the database need
        // not match prose.
        self.log_event(
            Some(session.id()),
            "turn_end",
            &serde_json::json!({
                "outcome": outcome,
                "steps": steps,
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "answer": answer,
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
                outcome: Some(outcome),
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

        let outcome = loop {
            // A `cancel` ends the turn here, where it can be stored whole.
            if cancel.is_cancelled() {
                break (TurnOutcome::Cancelled, step);
            }
            // Step boundary: a user line between a call and its result is rejected, so inject here.
            self.deliver_inbox(id, &name);
            // Read here every step: a `set_session` landing mid-turn is meant for the
            // next request.
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
            self.maybe_request_live_compact(id, &name);
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
            None => anyhow::bail!("{name} states no model: set_session <session> <model>"),
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
        notice_budget(
            self.compress_above_tokens,
            self.context_limit_percent,
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

    /// Queues one durable agent-written checkpoint request for this token interval.
    fn maybe_request_live_compact(&self, id: i64, name: &str) {
        if !self.live_compact_enabled {
            return;
        }
        let interval = self.live_compact_interval_tokens;
        let Ok(measurement) = self.store.live_compact_measurement(id) else {
            return;
        };
        if measurement.requested {
            return;
        }
        let Some(baseline) = measurement.baseline_tokens else {
            return;
        };
        if measurement.tokens.saturating_sub(baseline) < interval {
            return;
        }
        let message = live_compact_notice();
        match self
            .store
            .request_live_compact(id, name, measurement.generation, &message)
        {
            Ok(true) => {
                self.log_event(
                    Some(id),
                    "live_compact_notice",
                    &json!({
                        "tokens": measurement.tokens,
                        "baseline": baseline,
                        "interval": interval,
                        "generation": measurement.generation,
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
        source: crate::meta::Source<'_>,
    ) -> anyhow::Result<crate::store::Prompt> {
        // All three "where"s come from the session: its name, a stated placement, the
        // peer.
        let name = session.name();
        let placed = session.placed();
        let placed = placed.as_deref();
        let remote = session.remote_name();
        let remote = remote.as_deref();
        // Where it stands now, which is what the card and the index are about.
        let here = match placed {
            Some(path) => crate::session::stated(path),
            None => crate::session::stated(cwd),
        };
        // Stamped once, and again only on a move: the row records the directory
        // and the peer it was written for.
        if let Some(kept) = self.store.prompt(id)?
            && kept.cwd == here
            && kept.remote.as_deref() == remote
            && kept.model.as_deref() == Some(model)
        {
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
        // Where the conversation's references stand now, planned once, for the one
        // request that carries it.
        let (index, rooted) = match self.index_of(id, name, model, &here, &home, source).await {
            Some((index, rooted)) => (Some(index), rooted),
            None => (None, Vec::new()),
        };
        let prompt = crate::store::Prompt {
            instructions,
            index,
            tail,
            rooted,
            cwd: here,
            remote: remote.map(str::to_string),
            model: Some(model.to_string()),
        };
        self.store.append_prompt(id, &prompt)?;
        Ok(prompt)
    }

    /// How much an indexed entry may change before it is re-sent, or nothing when
    /// no patch may be appended.
    ///
    /// # Invariant
    ///
    /// A patch is appended only where the prose introducing it exists: a diff with no
    /// template behind it would arrive as an unexplained second copy of the entry.
    fn index_tolerance(&self) -> Option<f64> {
        let prompts = crate::prompts::shared();
        let explained = prompts.text(crate::prompts::REVISION).is_some();
        explained
            .then_some(self.index_change_tolerance)
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
        let entry = self.providers.place(model);
        let window = tier
            .and_then(|name| entry.as_ref().and_then(|e| e.cache_ttl_tiers.get(name)))
            .map(|secs| secs.saturating_mul(1_000))
            .or(self.cache_ttl_ms)
            .or_else(|| {
                entry
                    .as_ref()
                    .and_then(|e| e.cache_ttl_secs)
                    .map(|secs| secs.saturating_mul(1_000))
            });
        let pct = 100_u64.saturating_sub(self.model_config.cache_margin_pct as u64);
        window.unwrap_or(0).saturating_mul(pct) / 100
    }

    /// Runs one tool only until the short tier's window is nearly spent.
    ///
    /// # Invariant
    ///
    /// The interrupt cancels a child token, never the turn: a tool that was
    /// waiting returns with its background jobs still running, and the next
    /// request goes out while the prefix is still warm. See SPEC, `check_bash`.
    async fn run_before_cache_expiry(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &crate::tools::ToolContext,
        cancel: &tokio_util::sync::CancellationToken,
        model: &str,
        tier: Option<&str>,
        session_id: i64,
    ) -> crate::tools::ToolOutcome {
        let fire_after = self.cache_fire_after_ms(model, tier);
        if fire_after == 0 {
            return self.tools.run(name, args, ctx).await;
        }
        let since = self
            .store
            .last_usage_at(session_id)
            .ok()
            .flatten()
            .map(|at| crate::store::now_millis().saturating_sub(at).max(0) as u64)
            .unwrap_or(0);
        let remaining = fire_after.saturating_sub(since);
        let run = self.tools.run(name, args, ctx);
        tokio::pin!(run);
        if remaining == 0 {
            cancel.cancel();
            return run.await;
        }
        tokio::select! {
            outcome = &mut run => outcome,
            _ = tokio::time::sleep(std::time::Duration::from_millis(remaining)) => {
                cancel.cancel();
                run.await
            }
        }
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
        crate::prefix_cache::Rates {
            price: entry.as_ref().and_then(|entry| entry.price),
            invalid: observed.unwrap_or(self.cache_invalid_rate),
            ttl_ms: self.cache_ttl_ms.or(stated).unwrap_or(0),
        }
    }

    /// The index this request carries: what every reference says now, cheapest first.
    ///
    /// # Invariant
    ///
    /// The order is `prefix_cache`'s, a permutation of what it was given: no entry is
    /// invented for a reference that could not be read, and the spellings returned are the
    /// ones the index carries, so a message naming one again keys the same entry. Everything
    /// is read in `home`, the session's directory. See `docs/notes/server-agent.md`.
    async fn index_of(
        &self,
        id: i64,
        name: &str,
        model: &str,
        owner: &str,
        home: &std::path::Path,
        source: crate::meta::Source<'_>,
    ) -> Option<(String, Vec<String>)> {
        // The queue as well as the transcript: an early message left its `@file(...)`
        // mentions for this row to index.
        let mut texts = self.store.user_texts(id).ok()?;
        texts.extend(self.store.queued_texts(name).unwrap_or_default());
        let borrowed: Vec<&str> = texts.iter().map(String::as_str).collect();
        let roots = crate::meta::roots(source, home, &borrowed).await;
        if roots.is_empty() {
            return None;
        }
        let rates = self.rates(model);
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
            "prompt index",
            &wanted,
            rates,
            self.index_tolerance(),
        );
        let body = planned
            .entries
            .iter()
            .map(|(spelling, text)| format!("{spelling}\n{text}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let prompts = crate::prompts::shared();
        let mut index = prompts.render("reference_index", &[("entries", &body)]);
        if !planned.revisions.is_empty() {
            let changes = planned
                .revisions
                .iter()
                .map(|(spelling, patch)| format!("{spelling}\n{patch}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            index.push_str("\n\n");
            index.push_str(&prompts.render(crate::prompts::REVISION, &[("revisions", &changes)]));
        }
        let rooted: Vec<String> = planned.entries.into_iter().map(|(key, _)| key).collect();
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
            tier,
        } = *attempt;
        let generation = self.store.context_generation(id)?;
        let phase_started = std::time::Instant::now();
        let mut definitions = self.tools.definitions(session.on_remote());
        // A durable request alone makes the compaction schema visible to the model.
        if self.live_compact_enabled
            && self
                .store
                .live_compact_measurement(id)
                .is_ok_and(|measurement| measurement.requested)
            && let Some(definition) = self.tools.definition(
                crate::tools::live_compact::LIVE_COMPACT_TOOL,
                session.on_remote(),
            )
        {
            definitions.push(definition);
        }
        let reader = session.reader();
        let source = match (&reader, session.owner()) {
            (Some(reader), crate::session::Machine::Remote) => crate::meta::Source::Remote(reader),
            (None, crate::session::Machine::Remote) => crate::meta::Source::Unreachable,
            (_, crate::session::Machine::Local) => crate::meta::Source::Local,
        };
        let prompt = self
            .prompt_in_force(session, id, llm.model(), cwd, source)
            .await?;
        phases.prompt_ms = phase_started.elapsed().as_millis() as u64;
        // Pinned here, so every retry of this request replays the window that was
        // current when it started.
        let phase_started = std::time::Instant::now();
        let source = self.store.request_source(id)?;
        phases.source_ms = phase_started.elapsed().as_millis() as u64;
        // Where a failed attempt's rows end: the retry inside `llm.stream` sends this
        // window. See `docs/notes/server-agent.md`.
        let window = source.upto_seq;
        // One number for "how long the model took": the whole stream, retries included.
        let requested = std::time::Instant::now();
        // SPEC's ttft and tps both count from the first token: the wait for it,
        // then what came out over the time after.
        let mut first_token: Option<std::time::Instant> = None;
        phases.llm_started = Some(requested);
        // One tier for the whole turn, settled before its first request.
        let tier = self.cache_tier(llm, tier);
        // The directory, not the session name, is the unit that shares a prefix.
        let affinity = if prompt.cwd.is_empty() {
            name.to_string()
        } else {
            prompt.cwd.clone()
        };
        let mut rx = llm.stream(
            &prompt.instructions,
            prompt.index.as_deref(),
            prompt.tail.as_deref(),
            &source,
            &affinity,
            &definitions,
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
        let mut streamed: std::collections::HashMap<String, (ResponsePart, String)> =
            Default::default();
        // When each draft was last written, so a fast stream is not one write per chunk.
        let mut drafted: std::collections::HashMap<String, std::time::Instant> = Default::default();
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
                    let whole = streamed
                        .entry(item_id.clone())
                        .or_insert_with(|| (part, String::new()));
                    whole.1.push_str(&text);
                    // The draft is what survives a process that never finishes this
                    // item: the reader has already seen these chunks.
                    let due = drafted
                        .get(&item_id)
                        .is_none_or(|at| at.elapsed() >= DRAFT_FLUSH);
                    if due && text_worth_keeping(part) {
                        drafted.insert(item_id.clone(), std::time::Instant::now());
                        let _ =
                            self.store
                                .save_draft(id, &item_id, turn, part_name(part), &whole.1);
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
                                part,
                                text: text.clone(),
                                done: false,
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
                        part,
                        text: String::new(),
                        done: true,
                        call_id,
                    }));
                }
                LlmEvent::Reasoning {
                    item_id,
                    item,
                    summary,
                } => {
                    // The item, not a summary of it; see `docs/notes/server-agent.md`.
                    let payload = json!({"item": item, "summary": summary, "item_id": item_id});
                    said.insert(item_id.clone());
                    streamed.remove(&item_id);
                    self.settle_item(session, &item_id);
                    self.announce_row(name, self.store.append_message(id, "reasoning", &payload));
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
                            "builtin_call",
                            &json!({"item": item, "replayable": replayable}),
                        ),
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
                    self.announce_row(
                        name,
                        self.store
                            .append_message(id, "notice", &json!({"text": text})),
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
                                "text",
                                &json!({"role":"assistant","text":text}),
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
                    // Before it runs: this id is what the result carries, which
                    // is how a front-end holds a call and its output as one item.
                    self.bus.publish(Event::ToolCall(protocol::ToolCallEvent {
                        session: name.to_string(),
                        call_id: call_id.clone(),
                        head: call_head(&tool, &arguments),
                    }));
                    self.announce_row(
                        name,
                        self.store.append_message(
                            id,
                            "tool_call",
                            &json!({"call_id": call_id, "name": tool, "arguments": arguments,
                                    "step": step}),
                        ),
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
                    // The index it carried is warm again -- and only that one. The
                    // owner is the directory the prompt row states.
                    crate::prefix_cache::confirm(
                        &self.store,
                        &prompt.cwd,
                        "prompt index",
                        &prompt.rooted,
                    );
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
                        // Read before this answer is stored: the last input is
                        // the largest prefix this one could hit.
                        let expected = self.store.context_tokens(id).ok();
                        let gap_ms =
                            self.store.last_usage_at(id).ok().flatten().map(|at| {
                                crate::store::now_millis().saturating_sub(at).max(0) as u64
                            });
                        let hit_ratio = match (cached, expected) {
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
                                "input": u.input_tokens,
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
                    if let Some(input_tokens) = usage.as_ref().and_then(|u| u.input_tokens) {
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
            // One line for the operator, in the retries' voice; taken-for-an-answer text
            // is never resumed.
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
            self.keep_streamed(session, id, &name, turn, &mut streamed, &calls);
            return Ok(StepResult::Stopped);
        }
        if !ran_tools {
            return Ok(StepResult::Finished);
        }
        tracing::debug!(session = %name, step, "step ran its calls as they arrived");
        Ok(StepResult::RanTools)
    }

    /// Turns the `@image(...)` references in a tool's output into attachments.
    ///
    /// # Invariant
    ///
    /// A reference that names no readable picture is left as text, so a command
    /// printing one cannot fail the turn; a model that cannot take images is told
    /// so once, in the output, rather than having them silently dropped.
    async fn attach_references(
        &self,
        ctx: &ToolContext,
        printed_on: crate::session::Machine,
        output: &str,
    ) -> Vec<crate::image::ImageRef> {
        let mut out = Vec::new();
        // The bytes are on the machine that printed the name. See
        // `docs/notes/server-meta.md`.
        let reader = ctx.reader();
        let source = match (&reader, printed_on) {
            (Some(reader), crate::session::Machine::Remote) => crate::meta::Source::Remote(reader),
            (None, crate::session::Machine::Remote) => crate::meta::Source::Unreachable,
            (_, crate::session::Machine::Local) => crate::meta::Source::Local,
        };
        let dir = ctx.dir(printed_on);
        let named = crate::meta::mentions(output)
            .into_iter()
            .filter(|m| m.kind == protocol::Reference::Image)
            .map(|m| m.value);
        for given in named {
            // Read on the machine that printed it: a `bash` picture here, a `remote_bash`
            // one there.
            let path = source.spelling(&dir, &given);
            let bytes = match source.read(&dir, &given).await {
                Some(b) => b,
                // Not a picture this harness can read: leave the text alone.
                None => continue,
            };
            let Some(media) = crate::image::media_type(std::path::Path::new(&path), &bytes) else {
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
            let mut image = crate::image::ImageRef::new(path, media);
            // Stored too: nothing is uploaded here, the provider's file is resolved per
            // request.
            image.copy =
                crate::image::pictures::remember(&self.store, &image.path, media, &bytes).ok();
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
            // Its row exists and a provider wants it answered; a row the body would
            // otherwise invent is written once instead.
            self.answer_interrupted(id, name, &call_id, &tool, step);
            return true;
        }
        let model = llm.model().to_string();
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
        // SPEC: a timed-out tool leaves its job running; only the wait is dropped.
        let tool_cancel = cancel.child_token();
        let ctx = ToolContext {
            session: name.to_string(),
            call_id: call_id.clone(),
            home: self.home.clone(),
            output_dir: self.output_dir.clone(),
            shells: session.shells(),
            pythons: session.pythons(),
            reader: session.reader(),
            machine,
            remote: session.remote_name(),
            detached: self.detached.clone(),
            jobs: self.jobs.clone(),
            events: Some(tx),
            cancel: tool_cancel.clone(),
            store: self.store.clone(),
            cwd: session.cwd(),
            env: session.env(),
            vision: llm.vision(crate::prompts::shared()),
            inbox: session.inbox_signal(),
            holds_the_turn: true,
            stop: session.stop_signal(),
            closing: session.closing_signal(),
            explore: session.explore(),
        };
        let run_started = std::time::Instant::now();
        tracing::info!(session = %name, tool = %tool, "tool start");
        // Read before the run moves the arguments: the adapter's general
        // parameters title the call and cap its result.
        let call_description = description_of(&arguments).to_string();
        let max_output = arguments
            .get("max_output")
            .and_then(serde_json::Value::as_u64)
            .map(|max| max as usize);
        // Whose pictures these are: the machine the call's arguments name, if any.
        // See `docs/notes/server-remote.md`.
        let printed_on = self.tools.machine_of(&tool, &arguments, &ctx);
        let stated_tier = session.tuning().cache_tier;
        let outcome = self
            .run_before_cache_expiry(
                &tool,
                arguments,
                &ctx,
                &tool_cancel,
                &model,
                stated_tier.as_deref(),
                id,
            )
            .await;
        let run_ms = run_started.elapsed().as_millis() as u64;
        let attach_started = std::time::Instant::now();
        let scanned = self
            .attach_references(&ctx, printed_on, &outcome.output)
            .await;
        let attach_ms = attach_started.elapsed().as_millis() as u64;
        // SPEC's `max_output`: over the cap the output spills, and the model
        // reads the file's metadata.
        let capped = max_output
            .and_then(|max| {
                crate::tools::cap_max_output(
                    &ctx,
                    &tool,
                    (!call_description.is_empty()).then_some(call_description.as_str()),
                    &call_id,
                    &outcome.output,
                    max,
                )
            })
            .unwrap_or_else(|| outcome.output.clone());
        // A tool that streamed nothing owes its output to the final chunk.
        let compacting = tool == crate::tools::live_compact::LIVE_COMPACT_TOOL;
        let mut tail = match sent.load(Ordering::Relaxed) {
            0 if compacting => outcome.output.clone(),
            0 => capped.clone(),
            _ => String::new(),
        };
        // A checkpoint artifact is exactly the agent's summary, not a timing annotation.
        let mut output = if compacting {
            outcome.output.clone()
        } else {
            // The model is shown no head and no label, so the time ends the text it reads.
            with_time(capped, run_ms)
        };
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
        let mut content = json!({"call_id": call_id, "output": output, "images": images,
                    "step": step, "ok": outcome.ok, "elapsed_ms": run_ms});
        let mut checkpointed = false;
        let mut checkpoint_error = None;
        if tool == crate::tools::live_compact::LIVE_COMPACT_TOOL && outcome.ok {
            match self
                .store
                .append_live_compact_result(id, &call_id, &content)
            {
                Ok(committed) => {
                    checkpointed = true;
                    self.announce_row(name, Ok(committed.result_seq));
                    self.log_event(
                        Some(id),
                        "live_compact",
                        &json!({
                            "checkpoint": committed.checkpoint_id,
                            "result_seq": committed.result_seq,
                            "generation": committed.generation,
                        }),
                    );
                }
                Err(e) => {
                    tracing::warn!(session = %name, error = %e, "could not commit live compact checkpoint");
                    checkpoint_error = Some(format!("{e:#}"));
                }
            }
        }
        if tool == crate::tools::live_compact::LIVE_COMPACT_TOOL && !checkpointed {
            status = "error";
            let reason = checkpoint_error.unwrap_or_else(|| output.clone());
            output = format!("ERROR: live_compact checkpoint was not committed: {reason}");
            content["output"] = json!(output.clone());
            content["ok"] = json!(false);
            self.announce_row(name, self.store.append_message(id, "tool_result", &content));
            let _ = self.store.reset_live_compact_request(id);
            tail = output.clone();
        } else if !checkpointed {
            self.announce_row(name, self.store.append_message(id, "tool_result", &content));
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
        false
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
    fn answer_interrupted(&self, id: i64, name: &str, call_id: &str, tool: &str, step: u32) {
        let said = crate::store::interrupted_call();
        self.announce_row(
            name,
            self.store.append_message(
                id,
                "tool_result",
                &json!({
                    "call_id": call_id,
                    "output": said,
                    "images": [],
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
    /// the next start under an `interrupted` flag that is no longer true.
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
            self.bus.publish(Event::Response(ResponseEvent {
                session: session.to_string(),
                turn: protocol::REPLAY_TURN,
                part: ResponsePart::Input,
                text: line.clone(),
                done: true,
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
        streamed: &mut std::collections::HashMap<String, (ResponsePart, String)>,
        calls: &std::collections::HashMap<String, String>,
    ) {
        for (item_id, (part, text)) in streamed.drain() {
            if !text.trim().is_empty() {
                let stored = match part {
                    ResponsePart::Output => Some((
                        "text",
                        json!({"role": "assistant", "text": text, "interrupted": true}),
                    )),
                    ResponsePart::Reasoning => Some((
                        "reasoning",
                        json!({"summary": text, "item_id": item_id.clone(),
                               "interrupted": true}),
                    )),
                    // A call's text is its arguments; an activity is progress;
                    // the operator's words are not the model's to store.
                    ResponsePart::ToolCall | ResponsePart::Activity | ResponsePart::Input => None,
                };
                if let Some((kind, payload)) = stored {
                    self.announce_row(name, self.store.append_message(id, kind, &payload));
                }
            }
            self.bus.publish(Event::Response(ResponseEvent {
                session: name.to_string(),
                turn,
                part,
                text: String::new(),
                done: true,
                call_id: calls.get(&item_id).cloned(),
            }));
            self.settle_item(session, &item_id);
        }
    }

    /// Reports a failure, and leaves the reason somewhere it can be read later.
    ///
    /// # Invariant
    ///
    /// The line goes into the transcript as a `notice`, which no request is built
    /// from. An `Event::Log` alone is broadcast and forgotten: "why did that
    /// turn fail" was unanswerable a minute later, and the failure of the
    /// *first* attempt of a request exists nowhere else at all.
    /// Appends the row that answers a refusal to continue a step written without thinking.
    ///
    /// # Invariant
    ///
    /// Called only after the provider refused, so a session the provider accepts never
    /// carries one; see `docs/notes/server-agent.md`.
    fn append_thinking_mark(&self, id: i64, session: &str) {
        match self.store.append_message(
            id,
            "thinking_mark",
            &json!({"text": crate::store::THINKING_MARK}),
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

    fn error(&self, id: i64, session: &str, what: &str, e: anyhow::Error) {
        tracing::error!(session = %session, "failed to {what}: {e}");
        let said = format!("{session}: failed to {what}: {e}");
        self.announce_row(
            session,
            self.store
                .append_message(id, "notice", &json!({"text": said})),
        );
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

fn live_compact_notice() -> String {
    "The live-compaction token interval has elapsed. Call `live_compact` now with a      `summary` of everything since the previous checkpoint, including the latest complete      ordinary tool-call round. The server retains that round and every checkpoint summary;      do not use heir or delete any transcript rows."
        .to_string()
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
        let notice = context_notice(&notice_prompts(), "main", 700_000, 700_000);
        assert!(notice.contains("700000 input tokens"), "{notice}");
        assert!(notice.contains("--session main"), "{notice}");
        assert!(notice.contains("transcript.py read"), "{notice}");
        assert!(notice.contains("transcript.py heir"), "{notice}");
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
            .create_session_on("s", "m", ".", None)
            .expect("a session");
        let first = crate::store::Prompt {
            instructions: "prose".to_string(),
            index: None,
            tail: Some("Session `s`, 2026-11-01 14:00 UTC".to_string()),
            rooted: Vec::new(),
            cwd: ".".to_string(),
            remote: None,
            model: Some("m".to_string()),
        };
        store.append_prompt(id, &first).expect("one row");
        // A minute later: the stored card is what a request carries, not the clock.
        let read = store.prompt(id).expect("a readable row").expect("one row");
        assert_eq!(read.tail, first.tail);
        assert_eq!(read.instructions, "prose", "and nothing rewrote the prose");
    }

    fn notice_prompts() -> crate::prompts::Prompts {
        crate::prompts::Prompts::of(&[(
            "context_notice",
            "Context notice for `{{session}}`: {{tokens}} input tokens of {{budget}}.              Read `transcript.py read --session {{session}}`, hand over with              `transcript.py heir --session {{session}} --mentor <name> --note <text>`.",
        )])
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

    /// The notice names the two numbers it can know: the measured size and the budget.
    #[test]
    fn the_notice_names_the_size_and_the_budget() {
        let notice = context_notice(&notice_prompts(), "main", 10, 20);
        assert!(notice.contains("10 input tokens of 20"), "{notice}");
        assert!(!notice.contains("{{"), "{notice}");
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
    protocol::Head::call(tool, description_of(&value), call_fields(tool, &value))
}

/// What the call is for, in the model's own words: the one argument read as a title.
///
/// # Invariant
///
/// It is never a [`protocol::Field`]: an argument with no label of its own would be
/// spelled a second time under a name the row already said. Only ever a title -- a
/// missing or nonsensical one costs an untitled row, not a failed call.
fn description_of(value: &serde_json::Value) -> &str {
    value
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
}

/// A call's arguments, each marked with whether SPEC's `abstract` shows it.
///
/// # Invariant
///
/// The per-tool list is this crate's: it owns the tools, so it knows a `bash`
/// call is summarised by `{name, background, detached, timeout}` and that
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
        // SPEC lists bash's own set; another tool is summarised by nothing yet,
        // which still reads better than a wall of JSON.
        "bash" => &["name", "background", "time_out"],
        "check_job" => &["job_id", "wait_ms"],
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
    }
}
