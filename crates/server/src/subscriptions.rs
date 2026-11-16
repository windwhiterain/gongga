//! SPEC's `subscribe`: a named watch over the event stream.
//!
//! See `docs/notes/server-subscriptions.md`.
//!
//! # Invariant
//!
//! Subscribing registers and answers at once, and a match runs the commands the row
//! carries, in order, in the session that armed it -- nothing else. The server tells
//! nobody by itself: a subscription that must reach a session runs `send_message`,
//! which is why the news survives a restart as the pending row that command writes.

use std::sync::Arc;

use serde_json::json;

use crate::bus::EventBus;
use crate::store::{Store, SubscriptionRow};

/// What a subscription runs on a match: one command, or several in order.
///
/// # Invariant
///
/// The commands are a sequence, not a batch: the first refusal stops them, so a later
/// one may count on the ones before it having run. Every command goes through the
/// command layer -- the same door the `gongga_cli` tool uses -- so a subscription can
/// do exactly what its session's own call can, and nothing else. Its address is the
/// record it writes, not a program of its own.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Action {
    /// The commands, in the order a match runs them.
    pub cmds: Vec<Command>,
}

/// One command of an action: what it is called, and what it is told.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Command {
    /// The command's name, as the catalog spells it.
    pub name: String,
    /// Its named arguments, before the matched event is put into them.
    #[serde(default)]
    pub args: protocol::Args,
}

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
    /// `stop` is pulled only after `serve` returns, not when `cancel` is asked: the
    /// notice window still publishes events a subscriber is owed, and closing the
    /// matcher on the ask would lose them. This waits for the matcher loop, not for
    /// the actions it spawns. See `docs/notes/server-subscriptions.md`.
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
    /// The send is the whole of what a match does, and it runs as a detached task
    /// the stop does not wait for. What survives a stop is the durable pending row
    /// `send_message` writes, which the next start delivers -- not this task. See
    /// `docs/notes/server-subscriptions.md`.
    async fn run_action(
        self: Arc<Self>,
        row: SubscriptionRow,
        action: Action,
        event: protocol::Event,
    ) {
        let referent = referent(&event);
        let names: Vec<&str> = action.cmds.iter().map(|cmd| cmd.name.as_str()).collect();
        let (ok, said, ran) = self.run_cmds(&row.session, &action, &referent).await;
        let payload = json!({
            "subscription": row.name,
            "session": row.session,
            "event": event.name(),
            "cmds": names,
            "ran": ran,
            "ok": ok,
            "output": said,
        });
        self.log(&row, &payload);
    }

    /// Runs an action's commands in order, and answers how far it got.
    ///
    /// # Invariant
    ///
    /// `ran` counts the commands that finished, so `ran < cmds.len()` says the one at
    /// index `ran` stopped the sequence -- by a refusal, or by naming a field the
    /// matched event does not have. What that command said is the output.
    async fn run_cmds(
        &self,
        session: &str,
        action: &Action,
        referent: &serde_json::Value,
    ) -> (bool, String, usize) {
        let mut said = String::new();
        for (ran, cmd) in action.cmds.iter().enumerate() {
            let args = match expand_args(&cmd.args, referent) {
                Ok(args) => args,
                Err(why) => return (false, why, ran),
            };
            match self.run_cmd(session, &cmd.name, args).await {
                Ok(outcome) if outcome.ok => said = outcome.message,
                Ok(outcome) => return (false, outcome.message, ran),
                Err(why) => return (false, why, ran),
            }
        }
        (true, said, action.cmds.len())
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
        name: &str,
        args: protocol::Args,
    ) -> Result<crate::commands::Outcome, String> {
        let layer = crate::commands::layer()
            .ok_or_else(|| "this process has no command layer to run a match in".to_string())?;
        Ok(layer.run(session, name, args).await)
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
    /// What a row carries, when it carries anything: one command, or several.
    ///
    /// # Invariant
    ///
    /// Three shapes read as this one, because a home armed before the list existed keeps
    /// its rows: `{"name": .., "args": ..}` is that one command, `{"cmds": [..]}` is the
    /// list, and a bare array is the list too. A command with no name is dropped -- it
    /// would fire and do nothing, which is the one failure a registry cannot notice.
    fn of(row: &SubscriptionRow) -> Option<Action> {
        let stored = row.action.as_deref()?.trim();
        if stored.is_empty() {
            return None;
        }
        let cmds = serde_json::from_str::<Vec<Command>>(stored)
            .or_else(|_| serde_json::from_str::<Action>(stored).map(|action| action.cmds))
            .or_else(|_| serde_json::from_str::<Command>(stored).map(|one| vec![one]))
            .ok()?;
        let cmds: Vec<Command> = cmds
            .into_iter()
            .filter(|cmd| !cmd.name.trim().is_empty())
            .collect();
        (!cmds.is_empty()).then_some(Action { cmds })
    }
}

/// What a command's string arguments may name: the event that matched.
///
/// # Invariant
///
/// A log row's free-form `payload` is lifted to the top level, so a `turn_end` answer is
/// `{event.answer}` and not `{event.payload.answer}`: a record is a kind plus fields, and
/// the fields are what a subscriber came for. `name` and `session` are always there, so an
/// action need not know which kind it watches to say whose it was.
fn referent(event: &protocol::Event) -> serde_json::Value {
    let Ok(serde_json::Value::Object(mut table)) = serde_json::to_value(event) else {
        return serde_json::Value::Object(serde_json::Map::new());
    };
    if let protocol::Event::Record { payload, .. } = event {
        if let Some(fields) = payload.as_object() {
            for (key, value) in fields {
                table.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }
    table.insert("name".to_string(), json!(event.name()));
    if let Some(session) = event.session() {
        table.insert("session".to_string(), json!(session));
    }
    serde_json::Value::Object(table)
}

/// One command's arguments, with the matched event put where they name it.
fn expand_args(
    args: &protocol::Args,
    referent: &serde_json::Value,
) -> Result<protocol::Args, String> {
    protocol::Args::from_json(expand(&args.as_json(), referent)?)
}

/// `{event.<path>}` replaced by that field of the matched event, wherever it stands.
///
/// # Invariant
///
/// Only that spelling is a reference: every other brace and every other word reaches the
/// command as written. A path the event does not have is a refusal rather than the literal
/// text, because the alternative is a message that quietly says `{event.answer}` where the
/// answer should be.
fn expand(
    value: &serde_json::Value,
    referent: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match value {
        serde_json::Value::String(text) => {
            Ok(serde_json::Value::String(expand_text(text, referent)?))
        }
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| expand(item, referent))
            .collect::<Result<Vec<_>, String>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(table) => table
            .iter()
            .map(|(key, value)| Ok((key.clone(), expand(value, referent)?)))
            .collect::<Result<serde_json::Map<String, serde_json::Value>, String>>()
            .map(serde_json::Value::Object),
        other => Ok(other.clone()),
    }
}

/// What one string argument becomes: its references replaced, its own text kept.
fn expand_text(text: &str, referent: &serde_json::Value) -> Result<String, String> {
    const OPEN: &str = "{event.";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let after = start + OPEN.len();
        // Unterminated `{event.` is text, not a refusal: half a message beats a match that never runs.
        let Some(width) = rest[after..].find('}') else {
            break;
        };
        out.push_str(&rest[..start]);
        let path = &rest[after..after + width];
        let Some(named) = field(referent, path) else {
            return Err(format!(
                "`{{event.{path}}}` names no field of the event that matched; it has: {}",
                fields_of(referent)
            ));
        };
        out.push_str(&word(named));
        rest = &rest[after + width + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The field one dot-separated path names, when the event has it.
fn field<'a>(referent: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.')
        .try_fold(referent, |node, key| node.get(key))
}

/// What a field reads as inside a string: its text, or its JSON when it is not one.
fn word(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The names an event does have, so a refusal can say what was available instead.
fn fields_of(referent: &serde_json::Value) -> String {
    let mut names: Vec<&str> = referent
        .as_object()
        .map(|table| table.keys().map(String::as_str).collect())
        .unwrap_or_default();
    names.sort_unstable();
    if names.is_empty() {
        "no fields".to_string()
    } else {
        names.join(", ")
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
