//! SPEC's `subscribe`: a named watch over the event stream.
//!
//! See `docs/notes/server-subscriptions.md`.
//!
//! # Invariant
//!
//! Subscribing registers and answers at once, and a match runs the command the row
//! carries, in the session that armed it -- nothing else. The server tells nobody by
//! itself: a subscription that must reach a session runs `send_message`, which is
//! why the news survives a restart as the pending row that command writes.

use std::sync::Arc;

use serde_json::json;

use crate::bus::EventBus;
use crate::store::{Store, SubscriptionRow};

/// What a subscription runs on a match: one command, in the subscriber's session.
///
/// # Invariant
///
/// A match runs the command layer -- the same door a tool's `fn <tool>(**args)` uses --
/// so a subscription can do exactly what a session's own call can, and nothing else.
/// Its address is the record it writes, not a program of its own.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Action {
    /// The command's name, as the catalog spells it.
    pub name: String,
    /// Its named arguments.
    #[serde(default)]
    pub args: protocol::Args,
}

/// The armed subscriptions, and the one task that matches events against them.
pub struct Subscriptions {
    store: Store,
    bus: EventBus,
    /// The same slot a turn takes: a stop waits for the action too.
    active: Arc<std::sync::atomic::AtomicUsize>,
    /// The database's rows, compiled: the pattern is parsed once, not per event.
    armed: tokio::sync::Mutex<Vec<Armed>>,
}

struct Armed {
    row: SubscriptionRow,
    pattern: regex::Regex,
}

impl Subscriptions {
    /// Reads what a previous run left armed, so a restart resumes where it stopped.
    pub fn load(
        store: Store,
        bus: EventBus,
        active: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Arc<Self> {
        let armed = match store.subscriptions() {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| compiled(&store, row))
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "could not read the armed subscriptions");
                Vec::new()
            }
        };
        Arc::new(Self {
            store,
            bus,
            active,
            armed: tokio::sync::Mutex::new(armed),
        })
    }

    /// Arms `name` for `session`, which the caller has already checked;
    /// arming a name again replaces the row it held.
    ///
    /// # Invariant
    ///
    /// The pattern is compiled before the row is written: a refusal must leave
    /// nothing behind, so an unreadable pattern is the command's answer rather
    /// than a row that can never fire.
    pub async fn arm(
        &self,
        session: &str,
        name: &str,
        pattern: &str,
        persistent: bool,
        action: Option<Action>,
    ) -> Result<SubscriptionRow, String> {
        let compiled = regex::Regex::new(pattern)
            .map_err(|e| format!("pattern {pattern:?} is not a valid regex: {e}"))?;
        let row = self
            .store
            .add_subscription(session, name, pattern, persistent, action.clone())
            .map_err(|e| format!("could not record the subscription: {e}"))?;
        let mut armed = self.armed.lock().await;
        armed.retain(|a| !(a.row.session == session && a.row.name == name));
        armed.push(Armed {
            row: row.clone(),
            pattern: compiled,
        });
        Ok(row)
    }

    /// Drops one named subscription of `session`; answers how many rows went.
    pub async fn disarm_name(&self, session: &str, name: &str) -> Result<usize, String> {
        let dropped = self
            .store
            .delete_subscription_name(session, name)
            .map_err(|e| format!("could not drop the subscription: {e}"))?;
        self.armed
            .lock()
            .await
            .retain(|a| !(a.row.session == session && a.row.name == name));
        Ok(dropped)
    }

    /// Drops what one session asked to be told about; a deleted session owes none.
    pub async fn forget_session(&self, session: &str) -> usize {
        let dropped = self.store.clear_subscriptions(session).unwrap_or(0);
        self.armed.lock().await.retain(|a| a.row.session != session);
        dropped
    }

    /// Runs the matcher until the server stops: one task per server, not per pattern.
    ///
    /// # Invariant
    ///
    /// `stop` is the end of the drain, not the ask that started it. A turn that
    /// ends while the server is stopping is exactly the news a subscriber is owed,
    /// and the ask arrives minutes before the last turn does; closing the matcher on
    /// the ask loses every event in between. See `docs/notes/server-subscriptions.md`.
    pub async fn watch(self: Arc<Self>, stop: tokio_util::sync::CancellationToken) {
        let mut events = self.bus.subscribe();
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                received = events.recv() => match received {
                    Ok(delta) => self.matched(&delta.event).await,
                    // A matcher that fell behind cannot know what it missed, and a
                    // notice nobody got is not a notice: say so once, per lag.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "subscriptions fell behind the event bus");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    }

    /// Hands one event to every armed pattern it matches.
    async fn matched(self: &Arc<Self>, event: &protocol::Event) {
        let fired: Vec<SubscriptionRow> = {
            let armed = self.armed.lock().await;
            armed
                .iter()
                .filter(|a| matches(&a.pattern, event))
                .map(|a| a.row.clone())
                .collect()
        };
        for row in fired {
            if !row.persistent {
                self.disarm(row.id).await;
            }
            if let Some(action) = Action::of(&row) {
                let this = Arc::clone(self);
                let event = event.clone();
                tokio::spawn(async move { this.run_action(row, action, event).await });
            }
        }
    }

    /// Sends what one fired row carries, and writes the outcome where `events`
    /// reads it.
    ///
    /// # Invariant
    ///
    /// The send is the whole of what a match does. It holds a slot in
    /// `Dispatch::active`, so a stop waits for it -- a `send_message` issued during
    /// a drain lands as a pending row instead of dying with the process.
    async fn run_action(
        self: Arc<Self>,
        row: SubscriptionRow,
        action: Action,
        event: protocol::Event,
    ) {
        let _slot = crate::commands::ActiveGuard::new(self.active.clone());
        let (ok, said) = match self.run_cmd(&row.session, &action).await {
            Ok(outcome) => (outcome.ok, outcome.message),
            Err(why) => (false, why),
        };
        let payload = json!({
            "subscription": row.name,
            "session": row.session,
            "event": event.name(),
            "cmd": action.name,
            "ok": ok,
            "output": said,
        });
        self.log(&row, &payload);
    }

    /// Runs one subscription's command in the session that armed it.
    ///
    /// # Invariant
    ///
    /// The command layer is the whole door: a name the catalog does not know is
    /// refused there, so a subscription cannot reach past what its session can call.
    async fn run_cmd(
        &self,
        session: &str,
        action: &Action,
    ) -> Result<crate::commands::Outcome, String> {
        let layer = crate::commands::layer()
            .ok_or_else(|| "this process has no command layer to run a match in".to_string())?;
        Ok(layer.run(session, &action.name, action.args.clone()).await)
    }

    /// Writes one outcome row and announces it, in that order.
    ///
    /// # Invariant
    ///
    /// The same rule as every other event-log row: on disk before anybody is
    /// told, so a matcher that reacts to a `Record` can always read the row it
    /// was told about. A failed write is a warning: the action already ran, and
    /// losing its row must not take the matcher with it.
    fn log(&self, row: &SubscriptionRow, payload: &serde_json::Value) {
        let session_id = self
            .store
            .get_session(&row.session)
            .ok()
            .flatten()
            .map(|session| session.id);
        let Ok(event) = self
            .store
            .append_event_row(session_id, "subscription", payload)
        else {
            tracing::warn!(
                subscription = row.id,
                "could not write a subscription outcome"
            );
            return;
        };
        self.bus.publish(protocol::Event::Record {
            id: event.id,
            seq: event.seq,
            session: row.session.clone().into(),
            kind: event.kind,
            payload: payload.clone(),
            at: event.created_at,
        });
    }

    async fn disarm(&self, id: i64) {
        let _ = self.store.delete_subscription(id);
        self.armed.lock().await.retain(|a| a.row.id != id);
    }
}

impl Action {
    /// What a row carries, when it carries anything: one command.
    fn of(row: &SubscriptionRow) -> Option<Action> {
        let stored = row.action.as_deref()?.trim();
        let action: Action = serde_json::from_str(stored).ok()?;
        (!action.name.trim().is_empty()).then_some(action)
    }
}

/// Whether one event is what a pattern is about.
///
/// # Invariant
///
/// The pattern is matched against the event's name, and -- when the event is about
/// a session -- against `name:session`. One pattern language everywhere: a regex,
/// unanchored, so `turn_end` also matches `turn_end:child/2`.
fn matches(pattern: &regex::Regex, event: &protocol::Event) -> bool {
    if pattern.is_match(event.name()) {
        return true;
    }
    match event.session() {
        Some(session) => pattern.is_match(&format!("{}:{}", event.name(), session)),
        None => false,
    }
}

fn compiled(store: &Store, row: SubscriptionRow) -> Option<Armed> {
    match regex::Regex::new(&row.pattern) {
        Ok(pattern) => Some(Armed { row, pattern }),
        Err(e) => {
            // Dropped here and in the database: it can never match.
            tracing::warn!(
                subscription = row.id, pattern = %row.pattern, error = %e,
                "dropping a subscription whose pattern does not compile"
            );
            let _ = store.delete_subscription(row.id);
            None
        }
    }
}
