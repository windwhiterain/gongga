//! SPEC's `subscribe`: a named watch over the event stream.
//!
//! See `docs/notes/server-subscriptions.md`.
//!
//! # Invariant
//!
//! Subscribing registers and answers at once, and a match runs the action the row
//! carries in the subscriber's own interpreter -- nothing else. The server tells
//! nobody by itself: a subscription that must reach a session says so in its
//! action (`send_message` to the caller), which is why the news survives a restart
//! as the pending row that action writes.

use std::sync::Arc;

use serde_json::json;

use crate::bus::EventBus;
use crate::session::SessionManager;
use crate::store::{Store, SubscriptionRow};

/// What a subscription runs on a match, in the session's own named
/// interpreter -- the same one the session's tools use.
#[derive(Debug, Clone)]
pub enum Action {
    Bash { name: String, cmd: String },
    Python { name: String, code: String },
}

/// The armed subscriptions, and the one task that matches events against them.
pub struct Subscriptions {
    store: Store,
    bus: EventBus,
    agent: Arc<crate::agent::Agent>,
    sessions: SessionManager,
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
        agent: Arc<crate::agent::Agent>,
        sessions: SessionManager,
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
            agent,
            sessions,
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

    /// Runs what one fired row carries, and writes the outcome where `events`
    /// reads it.
    ///
    /// # Invariant
    ///
    /// The action is the whole of what a match does: it runs the path a model's call
    /// runs, so a subscription and its session see the same shell. It holds a slot in
    /// `Dispatch::active`, so a stop waits for it -- a `send_message` issued during a
    /// drain lands as a pending row instead of dying with the process.
    async fn run_action(
        self: Arc<Self>,
        row: SubscriptionRow,
        action: Action,
        event: protocol::Event,
    ) {
        let _slot = crate::commands::ActiveGuard::new(self.active.clone());
        let stated = event_prefix(&event);
        let (tool, args) = match &action {
            Action::Bash { name, cmd } => (
                "bash",
                json!({"name": name, "command": format!("( {stated}{cmd} )")}),
            ),
            // A python action carries no event: assignments would stay in the REPL
            // afterwards, so the row is what it reads.
            Action::Python { name, code } => ("python", json!({"name": name, "code": code})),
        };
        let outcome = crate::tool_cmd::run(
            &self.agent,
            &self.sessions,
            &self.store,
            &row.session,
            tool,
            args,
        )
        .await;
        let payload = json!({
            "subscription": row.name,
            "session": row.session,
            "event": event.name(),
            "ok": outcome.ok,
            "output": outcome.message,
        });
        self.log(&row, &payload);
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
    /// What a row carries, when it carries anything.
    fn of(row: &SubscriptionRow) -> Option<Action> {
        match (&row.bash, &row.cmd, &row.python, &row.code) {
            (Some(name), Some(cmd), _, _) => Some(Action::Bash {
                name: name.clone(),
                cmd: cmd.clone(),
            }),
            (_, _, Some(name), Some(code)) => Some(Action::Python {
                name: name.clone(),
                code: code.clone(),
            }),
            // A half-written row names an interpreter and no code.
            _ => None,
        }
    }
}

/// Whether one event is what a pattern is about.
///
/// # Invariant
///
/// The pattern is matched against the event's name, and -- when the event is about
/// a session -- against `name:session`. One pattern language, the same one
/// `del_bash` and `list_bash` speak: a regex, unanchored, so `turn_end` also
/// matches `turn_end:child/2`.
fn matches(pattern: &regex::Regex, event: &protocol::Event) -> bool {
    if pattern.is_match(event.name()) {
        return true;
    }
    match event.session() {
        Some(session) => pattern.is_match(&format!("{}:{}", event.name(), session)),
        None => false,
    }
}

/// What fired, as assignments the action's own words can read.
///
/// # Invariant
///
/// Three statements ending in `;`, all inside one subshell. `VAR=x cmd "$VAR"` is the
/// shape that does not work: a simple command's words expand before its own
/// assignments land, so the command reads nothing. In a subshell the values are gone
/// when the action ends, and the status stays the action's own.
fn event_prefix(event: &protocol::Event) -> String {
    let kind = quote(event.name());
    let session = quote(event.session().unwrap_or_default());
    let body = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
    let body = serde_json::to_string(&body).unwrap_or_else(|_| body.to_string());
    format!(
        "GONGGA_EVENT_KIND={kind}; GONGGA_EVENT_SESSION={session}; GONGGA_EVENT_PAYLOAD={payload}; ",
        payload = quote(&body)
    )
}

/// One value as a shell sees it: single quotes, an inner quote closed and reopened.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
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
