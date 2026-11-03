//! SPEC's `subscribe`: a session told about an event it named.
//!
//! See `docs/notes/server-subscriptions.md`.
//!
//! # Invariant
//!
//! Subscribing registers and answers at once; the news becomes an inbox row for the
//! session that asked, so an idle one is woken by it.

use std::sync::Arc;

use serde_json::json;

use crate::bus::EventBus;
use crate::store::{Store, SubscriptionRow};

/// The armed subscriptions, and the one task that matches events against them.
pub struct Subscriptions {
    store: Store,
    bus: EventBus,
    /// The database's rows, compiled: the pattern is parsed once, not per event.
    armed: tokio::sync::Mutex<Vec<Armed>>,
}

struct Armed {
    row: SubscriptionRow,
    pattern: regex::Regex,
}

impl Subscriptions {
    /// Reads what a previous run left armed, so a restart resumes where it stopped.
    pub fn load(store: Store, bus: EventBus) -> Arc<Self> {
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
            armed: tokio::sync::Mutex::new(armed),
        })
    }

    /// Arms `pattern` for `session`, which the caller has already checked.
    ///
    /// # Invariant
    ///
    /// The pattern is compiled before the row is written: a refusal must leave
    /// nothing behind, so an unreadable pattern is the command's answer rather
    /// than a row that can never fire.
    pub async fn arm(
        &self,
        session: &str,
        pattern: &str,
        persistent: bool,
    ) -> Result<SubscriptionRow, String> {
        let compiled = regex::Regex::new(pattern)
            .map_err(|e| format!("--event {pattern:?} is not a valid regex: {e}"))?;
        let row = self
            .store
            .add_subscription(session, pattern, persistent)
            .map_err(|e| format!("could not record the subscription: {e}"))?;
        self.armed.lock().await.push(Armed {
            row: row.clone(),
            pattern: compiled,
        });
        Ok(row)
    }

    /// Drops what one session asked to be told about; a deleted session owes none.
    pub async fn forget_session(&self, session: &str) -> usize {
        let dropped = self.store.clear_subscriptions(session).unwrap_or(0);
        let mut armed = self.armed.lock().await;
        armed.retain(|a| a.row.session != session);
        dropped
    }

    /// Runs the matcher until the server stops: one task per server, not per pattern.
    pub async fn watch(self: Arc<Self>, stop: tokio_util::sync::CancellationToken) {
        let mut events = self.bus.subscribe();
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                received = events.recv() => match received {
                    Ok(event) => self.matched(&event).await,
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
    async fn matched(&self, event: &protocol::Event) {
        let fired: Vec<SubscriptionRow> = {
            let armed = self.armed.lock().await;
            armed
                .iter()
                .filter(|a| matches(&a.pattern, event))
                .map(|a| a.row.clone())
                .collect()
        };
        for row in fired {
            self.notice(&row, event).await;
            if !row.persistent {
                self.disarm(row.id).await;
            }
        }
    }

    /// Queues the news for the session that asked for it.
    async fn notice(&self, row: &SubscriptionRow, event: &protocol::Event) {
        // A session that is gone has nobody left to tell.
        match self.store.get_session(&row.session) {
            Ok(Some(_)) => {}
            Ok(None) => {
                self.disarm(row.id).await;
                return;
            }
            Err(e) => {
                tracing::warn!(session = %row.session, error = %e, "could not read a subscriber");
                return;
            }
        }
        // Not marked as a notice: this one is meant to wake an idle session.
        if let Err(e) = self.store.add_pending_task(
            &row.session,
            "inject",
            &json!({
                "session": row.session,
                "message": said(row, event),
                "subscription": row.id,
            }),
        ) {
            tracing::warn!(
                session = %row.session, subscription = row.id, error = %e,
                "could not queue a subscription's news"
            );
        }
    }

    async fn disarm(&self, id: i64) {
        let _ = self.store.delete_subscription(id);
        self.armed.lock().await.retain(|a| a.row.id != id);
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

/// What the subscriber reads when the news lands.
///
/// # Invariant
///
/// The row is passed through as it was published, and the header says which
/// pattern it answered: a session may hold several subscriptions, and "something
/// happened" without saying what was watched is not an answer.
fn said(row: &SubscriptionRow, event: &protocol::Event) -> String {
    let about = match event.session() {
        Some(session) => format!(" about {session}"),
        None => String::new(),
    };
    let body = match event {
        protocol::Event::Record { payload, id, .. } => {
            format!("log row {id}: {}", compact(payload))
        }
        other => compact(&serde_json::to_value(other).unwrap_or(serde_json::Value::Null)),
    };
    format!(
        "subscribe `{}` matched `{}`{about}.\n\n{body}",
        row.pattern,
        event.name()
    )
}

/// The body on one line: it rides the transcript, so punctuation is not free.
fn compact(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
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
