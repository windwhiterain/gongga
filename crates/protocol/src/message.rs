//! JSON event vocabulary, shared by the server, the CLI and the TUI.
//!
//! See `docs/notes/protocol-message.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

pub type Args = BTreeMap<String, String>;

// Client -> server

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message on any connection; answered with `Hello` or `Denied`.
    /// See `docs/notes/protocol-message.md`.
    Hello { client: ClientInfo, key: String },
    /// A command, already parsed and with sticky arguments applied.
    Command { id: u64, name: String, args: Args },
    /// Polite disconnect.
    Bye,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub version: String,
    /// The build of the program dialing: see [`crate::BUILD`].
    #[serde(default)]
    pub build: String,
    /// Free-form label shown by the server in logs, e.g. `cli/pid-1234`.
    pub label: String,
    /// The process that would own this server if nobody does.
    ///
    /// # Invariant
    ///
    /// A window sets it to its own pid, so a server it did not start still stops
    /// when that window closes; a one-shot command never sets it, because it exits
    /// in milliseconds and would take the server with it.
    /// See `docs/notes/protocol-message.md`.
    #[serde(default)]
    pub owner: Option<u32>,
}

// Server -> client

/// Adjacently tagged: `Event` carries its own `t`, so internal tagging would emit `t` twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Greeting plus the command catalog the CLI should complete against.
    Hello {
        protocol: u32,
        server: ServerInfo,
        commands: Vec<CommandSpec>,
        sessions: Vec<SessionInfo>,
    },
    /// The key was wrong. The connection is closed right after this.
    Denied { reason: String },
    /// A single streamed observable.
    Event(Event),
    /// An event whose `t` this build does not know, or whose shape it cannot read.
    ///
    /// # Invariant
    ///
    /// A new kind of event is not a breaking change. The wire is decoded by the
    /// binary that happens to be running, and a client built before an event
    /// existed would otherwise fail to parse the message and lose the whole
    /// connection -- which made every addition a lockstep upgrade of every
    /// front-end. Here the bytes are kept as they arrived: a reader that knows
    /// the kind prints it, one that does not shows it as unknown instead of
    /// dying. See `docs/notes/protocol-message.md`.
    Unknown {
        /// The event exactly as it arrived, so a front-end can pass it on.
        event: serde_json::Value,
        /// Why this build could not read it: what a reader needs to tell a newer
        /// server from a corrupt frame.
        why: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub version: String,
    /// The build of the program that owns this session: see [`crate::BUILD`].
    #[serde(default)]
    pub build: String,
    pub pid: u32,
    pub started_at: i64,
    /// The home this server owns, so a client can tell it reached its own.
    #[serde(default)]
    pub home: String,
    pub cwd: String,
    pub model: String,
    /// The process this server follows, if one does: a window that owns it dies,
    /// and the server stops. A reader compares it with its own pid to learn
    /// whether it is the window in charge. See `docs/notes/protocol-message.md`.
    #[serde(default)]
    pub owner: Option<u32>,
}

/// The `events[]` stream from the SPEC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Event {
    /// Echo of a command the server accepted for execution.
    Cmd { id: u64, name: String, args: Args },
    /// Terminal outcome of a command.
    CmdResult {
        id: u64,
        ok: bool,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
    },
    /// Streamed model response belonging to a session.
    Response(ResponseEvent),
    /// A tool the model asked for, as the call its answer will be addressed to.
    ToolCall(ToolCallEvent),
    /// Streamed tool output (bash stdout/stderr).
    ToolAnswer(ToolAnswerEvent),
    /// The set of sessions changed.
    Sessions { sessions: Vec<SessionInfo> },
    /// A session's run state changed.
    Status {
        session: String,
        status: SessionStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Diagnostic line for the TUI status bar.
    Log { level: LogLevel, message: String },
    /// What a session's last completed request measured. See [`UsageEvent`].
    Usage(UsageEvent),
    /// What is left at a provider, read at a step's end. See [`Quota`].
    Quota(Quota),
    /// One row of the server's event log, as the log keeps it.
    ///
    /// # Invariant
    ///
    /// The shape is `kind` plus a free-form `payload`, never a variant per kind.
    /// The log is append-only data -- `turn_end`, `step_timing`, `tool_result`,
    /// `usage`, and whatever a script emits tomorrow -- so a closed enum here
    /// would mean a wire change for every new kind, which is the cost this whole
    /// mechanism exists to remove. A reader switches on `kind` and ignores what it
    /// does not know.
    Record {
        /// The log's own id: a cursor, monotonic over the whole table.
        id: i64,
        /// Where this sits in the session's own sequence, when it has one.
        seq: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        kind: String,
        payload: serde_json::Value,
        /// Milliseconds since the epoch, as the log wrote it.
        at: i64,
    },
}

impl Event {
    /// The session this event is about, when it is about one.
    ///
    /// # Invariant
    ///
    /// Two kinds name no session -- a command and its answer, and the facts that
    /// belong to the whole server (the session list, a log line, a quota reading).
    /// Everything that belongs to a conversation says which, so a reader can tell
    /// one conversation's lines from another's without knowing the event's shape.
    pub fn session(&self) -> Option<&str> {
        match self {
            Event::Response(e) => Some(e.session.as_str()),
            Event::ToolCall(e) => Some(e.session.as_str()),
            Event::ToolAnswer(e) => Some(e.session.as_str()),
            Event::Status { session, .. } => Some(session.as_str()),
            Event::Usage(e) => Some(e.session.as_str()),
            Event::Record { session, .. } => session.as_deref(),
            Event::Cmd { .. } | Event::CmdResult { .. } | Event::Sessions { .. } => None,
            Event::Log { .. } | Event::Quota(_) => None,
        }
    }
}

/// SPEC's `active_quota` row: what the provider the last step ran on still allows.
///
/// # Invariant
///
/// Every figure is optional and absent means "this provider did not report it":
/// a month's share is not the same fact as a wallet balance, and a zero would
/// claim the allowance was spent. See `docs/notes/protocol-message.md`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    /// Roster provider id, e.g. `deepseek-official`.
    pub provider: String,
    /// Shares left of the three windows SPEC names, as percentages.
    pub month_percent: Option<f64>,
    pub week_percent: Option<f64>,
    pub five_hour_percent: Option<f64>,
    /// An allowance reported without a ceiling: a credit count, not a share.
    pub credit: Option<f64>,
    /// A wallet balance.
    pub money: Option<f64>,
    /// The symbol `money` is in: `¥` or `$`.
    pub currency: String,
    /// Why there is no reading, or what the provider flagged, in its own words.
    pub note: Option<String>,
}

/// One request, as the provider's own numbers and this server's clock measured it.
///
/// # Invariant
///
/// Sent per completed step, not per turn: a title that says "latest" has to be the
/// last measurement, and a delta keeps the list from being republished every step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub session: String,
    #[serde(flatten)]
    pub usage: Usage,
}

/// What one request measured, without saying which session made it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens the provider billed for the request; 0 is never measured.
    pub context_tokens: u64,
    /// What of those the provider served from its prefix cache; `None` is unreported.
    pub cached_tokens: Option<u64>,
    /// Output tokens per second over the stream, when the request produced any.
    pub tps: Option<f64>,
    /// Time from the request to its first streamed token, in milliseconds.
    pub ttft_ms: Option<u64>,
}

/// Which sub-stream of an assistant response a chunk belongs to.
/// See `docs/notes/protocol-message.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponsePart {
    Reasoning,
    ToolCall,
    Output,
    /// A tool the provider ran itself, and the sources it cited.
    Activity,
}

/// The turn a read re-sends a stored row under, which no live turn can use.
///
/// # Invariant
///
/// Turns count from 1, so `0` tells a front-end this is a row asked for by name:
/// the block it opens stands beside a live stream, never inside it.
pub const REPLAY_TURN: u64 = 0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEvent {
    pub session: String,
    /// Turns are numbered from 1 so the CLI can drop stale chunks after a
    /// cancel; a read uses `REPLAY_TURN`.
    pub turn: u64,
    pub part: ResponsePart,
    pub text: String,
    /// True on the final chunk for this part of this turn.
    pub done: bool,
    /// The call this part is streaming into, when it is a tool call.
    ///
    /// # Invariant
    ///
    /// A call streams under the id its result will carry, so a front-end draws one
    /// block for it from the first delta to the verdict, and the structured
    /// [`ToolCallEvent`] that follows fills the same block rather than a second one.
    #[serde(default)]
    pub call_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStream {
    Stdout,
    Stderr,
}

/// What a block is, and the words it introduces itself with.
///
/// # Invariant
///
/// The server says *what kind* of thing a row is and spells its text -- units,
/// verdicts and digits are knowledge only it has. It never says what that looks
/// like: the glyph and the colours are the front-end's, so restyling the window
/// is a change to one crate and nothing on the wire moves.
/// See `docs/notes/protocol-message.md`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Head {
    pub kind: HeadKind,
    /// The item's own name -- a tool's, for a call -- or empty when it has none.
    #[serde(default)]
    pub name: String,
    /// The item's arguments, each with the label it is read under.
    ///
    /// # Invariant
    ///
    /// One list, sent once: which fields summarise the item is `Field::summary`,
    /// named by the schema in `docs/notes/protocol-message.md` and filled in by
    /// whoever knows the tool. A front-end composes both rows from this, so
    /// expanding never asks the server again.
    #[serde(default)]
    pub fields: Vec<Field>,
}

/// One argument of an item, as its row reads it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// The argument's name, as SPEC spells it; empty for a value with no name.
    #[serde(default)]
    pub label: String,
    /// The value, already spelled: the server owns units and digits.
    pub value: String,
    /// Whether SPEC puts it in the `abstract`, the folded row.
    #[serde(default)]
    pub summary: bool,
}

impl Head {
    /// A head whose whole content is one value: a verdict, a duration.
    pub fn new(kind: HeadKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            name: String::new(),
            fields: vec![Field {
                label: String::new(),
                value: text.into(),
                summary: true,
            }],
        }
    }

    /// A tool call: its name, then the arguments the model gave it.
    pub fn call(name: impl Into<String>, fields: Vec<Field>) -> Self {
        Self {
            kind: HeadKind::Call,
            name: name.into(),
            fields,
        }
    }

    /// The folded row: SPEC's `abstract`, from the name and the marked fields.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.name.is_empty() {
            parts.push(self.name.clone());
        }
        parts.extend(self.fields.iter().filter(|f| f.summary).map(spelled));
        parts.join(", ")
    }

    /// The open item: one row per argument, the unnamed value first.
    pub fn detail(&self) -> String {
        let mut rows: Vec<String> = Vec::new();
        if !self.name.is_empty() {
            rows.push(self.name.clone());
        }
        rows.extend(self.fields.iter().map(spelled));
        rows.join("\n")
    }
}

/// One field as a row says it: `name=build`, or a bare value.
fn spelled(field: &Field) -> String {
    match field.label.is_empty() {
        true => field.value.clone(),
        false => format!("{}={}", field.label, field.value),
    }
}

/// Which kind of row a [`Head`] introduces, in SPEC's vocabulary.
///
/// # Invariant
///
/// A closed set the server names and the front-end maps to one look: an unknown
/// kind is not possible without a protocol change, which is what keeps "what
/// exists" and "how it is drawn" from drifting apart in two crates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadKind {
    /// A tool the model asked for.
    #[default]
    Call,
    /// A tool that finished and worked.
    Ok,
    /// A tool that finished and failed.
    Failed,
    /// The model's own reasoning, shown as its first and last line.
    Reasoning,
    /// The answer.
    Output,
    /// A command this session ran.
    Cmd,
    /// Something that went wrong.
    Error,
}

/// One tool the model asked for, as the row it is read on.
///
/// # Invariant
///
/// `call_id` is the id every one of its [`ToolAnswerEvent`]s carries, published
/// before the tool runs: that id is how a front-end joins a call to its result.
/// See `docs/notes/protocol-message.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallEvent {
    pub session: String,
    pub call_id: String,
    /// The call's arguments, each marked with whether SPEC's `abstract` shows it.
    pub head: Head,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAnswerEvent {
    pub session: String,
    pub call_id: String,
    pub tool: String,
    pub stream: ToolStream,
    pub text: String,
    pub done: bool,
    /// How the call ended, on the chunk that ends it: `None` while it runs.
    ///
    /// # Invariant
    ///
    /// Only the final chunk carries it: a stream that ended is the only place a
    /// result exists. The time is spelled here because the server measured it.
    #[serde(default)]
    pub head: Option<Head>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub status: SessionStatus,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: u64,
    pub cwd: String,
    /// The last measurement, so a window attached now draws it without a turn.
    #[serde(default)]
    pub usage: Usage,
    /// The model a turn here would spend on, `provider/model_name`; `None` is a
    /// server that does not know what it would send.
    #[serde(default)]
    pub model: Option<String>,
    /// Reasoning level in force, in the roster's spelling; `None` is the model's own.
    #[serde(default)]
    pub thinking_effort: Option<String>,
    /// The share of the window a context notice is sent at, when this session has one.
    #[serde(default)]
    pub context_limit: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// A sticky command argument; client-side only, never on the wire.
/// See `docs/notes/protocol-message.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CmdArg {
    pub name: String,
    pub value: String,
}

// Command catalog, published by the server and consumed by the CLI completer

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    pub args: Vec<ArgSpec>,
    /// Who runs it. Published by the server, so no front-end keeps a list.
    #[serde(default)]
    pub target: CommandTarget,
}

/// The lines `set_render --block_size` offers, in the order it lists them.
///
/// # Invariant
///
/// Any number is accepted; this is the set worth offering, not the set allowed.
pub const BLOCK_SIZES: &[&str] = &["0", "1", "2", "3", "5"];

/// The values `set_render_session --show` takes.
pub const SHOW_VALUES: &[&str] = &["true", "false"];

/// Where a command runs.
///
/// # Invariant
///
/// Every front-end dispatches on this, never on a name: a command added by a
/// script is runnable the moment the greeting names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandTarget {
    /// The server runs it in-process.
    #[default]
    Server,
    /// A script registered with the server runs it, on the server's behalf.
    ServerScript,
    /// The client runs it against its own state; it never reaches the server.
    Client,
}

/// The commands a client runs against its own state, published like any other.
///
/// # Invariant
///
/// No front-end dispatches on this list: each reads the `target` of what the
/// server sent. It lives here because the greeting is the contract, so a test
/// anywhere can stand in for a server by handing it over.
pub fn client_commands() -> Vec<CommandSpec> {
    [
        (
            "set_sticky",
            "Set a sticky argument, applied to every later command.",
            vec![
                spec_arg(
                    "name",
                    "Argument name, e.g. `session`.",
                    true,
                    Some("name"),
                    CompletionSource::Arguments,
                ),
                spec_arg(
                    "value",
                    "Value to apply when the flag is omitted. Completes the way the named \
                     argument does.",
                    true,
                    Some("value"),
                    CompletionSource::OfNamedArgument,
                ),
            ],
        ),
        (
            "wait",
            "Print the next row of the event log with this kind, then exit. A primitive, \
             not a policy: it does not know whether a turn has already finished, how long \
             is too long, or what a failed turn means. Bound it with `timeout` and decide \
             the rest in the caller.",
            vec![
                spec_arg(
                    "kind",
                    "The log row kind to wait for, e.g. `turn_end`, `test_failed`.",
                    true,
                    Some("kind"),
                    CompletionSource::None,
                ),
                spec_arg(
                    "session",
                    "Only a row about this session. A text filter: a name that does not \
                     exist is a filter that never matches, not an error.",
                    false,
                    Some("name"),
                    CompletionSource::Sessions,
                ),
            ],
        ),
        (
            "del_sticky",
            "Remove a sticky argument.",
            vec![spec_arg(
                "name",
                "Argument name to remove.",
                true,
                Some("name"),
                CompletionSource::StickyArguments,
            )],
        ),
        (
            "set_render",
            "How the events are drawn; with no argument, the setting it has.",
            vec![spec_arg(
                "block_size",
                "`0` caps nothing. Otherwise a non-answer block shows at most this many \
                 wrapped rows and keeps the newest of them, so a long stream scrolls \
                 inside its own block instead of pushing other blocks away.",
                false,
                Some("lines"),
                CompletionSource::Static(BLOCK_SIZES.iter().map(|m| m.to_string()).collect()),
            )],
        ),
        (
            "set_render_session",
            "Whether one session is shown beside the list; with no `--show`, what it is now.",
            vec![
                spec_arg(
                    "session",
                    "The session whose window this shows or hides.",
                    true,
                    Some("name"),
                    CompletionSource::Sessions,
                ),
                spec_arg(
                    "show",
                    "`true` gives the session a header and its events beside the list; \
                     `false` leaves it as a name in the list.",
                    false,
                    Some("true|false"),
                    CompletionSource::Static(SHOW_VALUES.iter().map(|m| m.to_string()).collect()),
                ),
            ],
        ),
        (
            "connect_server",
            "Point this client at a server, and remember it for later commands.",
            vec![
                spec_arg(
                    "ip",
                    "Where the server is; localhost when omitted.",
                    false,
                    Some("ip"),
                    CompletionSource::None,
                ),
                spec_arg(
                    "port",
                    "The UDP port it listens on.",
                    true,
                    Some("port"),
                    CompletionSource::None,
                ),
            ],
        ),
        (
            "serve",
            "Run a server for this home in this process, in the foreground.",
            vec![spec_arg(
                "port",
                "The UDP port to listen on, on localhost.",
                true,
                Some("port"),
                CompletionSource::None,
            )],
        ),
    ]
    .into_iter()
    .map(|(name, description, args)| CommandSpec {
        name: name.into(),
        description: description.into(),
        args,
        target: CommandTarget::Client,
    })
    .collect()
}

/// One argument of a published command.
fn spec_arg(
    name: &str,
    description: &str,
    required: bool,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> ArgSpec {
    ArgSpec {
        name: name.into(),
        description: description.into(),
        required,
        value_hint: value_hint.map(str::to_string),
        completions,
        sticky: false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgSpec {
    /// Name without leading dashes, e.g. `message`.
    pub name: String,
    pub description: String,
    pub required: bool,
    /// Shown as the value placeholder during completion.
    pub value_hint: Option<String>,
    /// Where the CLI should source completion candidates.
    #[serde(default)]
    pub completions: CompletionSource,
    /// Whether a sticky value may fill this argument when the caller omits it.
    ///
    /// # Invariant
    ///
    /// A required argument is filled from `Sticky` whether or not this is set, so
    /// the flag exists for the optional ones a reader thinks of as a mode rather
    /// than a fact about one command -- SPEC spells those `(sticky)`. Absence is
    /// still a decision everywhere else: a filter that keeps what it had must not
    /// be overwritten by a sticky value meant for a different command.
    #[serde(default)]
    pub sticky: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionSource {
    /// No completion; the value is free-form.
    #[default]
    None,
    /// Complete against the live session list.
    Sessions,
    /// Complete against a fixed set.
    Static(Vec<String>),
    /// Complete against every argument name; for values that *are* a name.
    /// See `docs/notes/protocol-message.md`.
    Arguments,
    /// Complete against the filesystem, relative to the client's cwd.
    /// See `docs/notes/protocol-message.md`.
    Paths,
    /// Complete this value the way the argument named by `--name` would be.
    /// See `docs/notes/protocol-message.md`.
    OfNamedArgument,
    /// Complete against the client's sticky argument names.
    ///
    /// # Invariant
    ///
    /// Client state stays client-side: the server names the source, the client
    /// fills it in, so publishing a client command needs no knowledge of it.
    StickyArguments,
    /// Complete against the skills installed on this machine.
    ///
    /// # Invariant
    ///
    /// The names are read where the server reads them -- the home's `skills/`
    /// and the machine's -- so what a reader completes is what can be opened.
    Skills,
}

// Encoding helpers

pub fn encode_client(msg: &ClientMessage) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(msg)
}

pub fn decode_client(bytes: &[u8]) -> Result<ClientMessage, serde_json::Error> {
    serde_json::from_slice(bytes)
}

pub fn encode_server(msg: &ServerMessage) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(msg)
}

/// Reads one message, keeping an event it does not recognise rather than failing.
///
/// # Invariant
///
/// Only a message that *is* an event falls back: the outer tag has to say
/// `event` and the payload has to be an object with a `t` of its own. Anything
/// else fails as it did before, because a corrupt frame is a real error and
/// hiding it would turn a bug into a silent gap.
///
/// A recognised tag whose fields this build cannot read falls back as well --
/// that is the same situation one step along: the server learned a required
/// field before this client did.
pub fn decode_server(bytes: &[u8]) -> Result<ServerMessage, serde_json::Error> {
    let error = match serde_json::from_slice::<ServerMessage>(bytes) {
        Ok(message) => return Ok(message),
        Err(error) => error,
    };
    match unrecognised_event(bytes) {
        Some(unknown) => Ok(unknown),
        None => Err(error),
    }
}

/// The event a build cannot read, if that is what these bytes are.
fn unrecognised_event(bytes: &[u8]) -> Option<ServerMessage> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if value.get("t").and_then(|t| t.as_str()) != Some("event") {
        return None;
    }
    let event = value.get("v")?.clone();
    // An event is an object with a tag of its own; anything else is not one.
    event.get("t").and_then(|t| t.as_str())?;
    Some(ServerMessage::Unknown {
        event,
        why: "this build does not know that kind of event".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A kind this build does not know must not cost the connection.
    ///
    /// # Invariant
    ///
    /// The wire is decoded by whichever binary is running, and a front-end built
    /// before an event existed cannot be rebuilt retroactively. Failing here would
    /// make every addition a lockstep upgrade -- which is the cost this test
    /// exists to stop anybody paying again.
    #[test]
    fn an_event_from_a_newer_server_is_kept_not_refused() {
        let wire = br#"{"t":"event","v":{"t":"quota_forecast","session":"main","left":3}}"#;
        let message = decode_server(wire).expect("an unknown kind is not a broken frame");
        match message {
            ServerMessage::Unknown { event, .. } => {
                assert_eq!(event["t"], "quota_forecast");
                assert_eq!(event["left"], 3, "and it is kept, not summarised away");
            }
            other => panic!("expected an unknown event, got {other:?}"),
        }
    }

    /// The fallback is for events only: a corrupt frame stays an error.
    ///
    /// # Invariant
    ///
    /// A tolerant decoder that accepts anything turns a bug into a silent gap. So
    /// the two things that make a message an event -- the outer tag and a tag of
    /// its own -- are both required before the bytes are kept.
    #[test]
    fn a_frame_that_is_not_an_event_still_fails() {
        // An outer tag nobody publishes.
        assert!(decode_server(br#"{"t":"nonsense","v":{}}"#).is_err());
        // An event with no tag of its own: malformed, not merely newer.
        assert!(decode_server(br#"{"t":"event","v":{"session":"main"}}"#).is_err());
        // A kind this build knows but a shape it cannot read is kept too: the day
        // a server adds a required field, an older client keeps the event instead
        // of losing the connection. It is still named, so a reader can tell.
        match decode_server(br#"{"t":"event","v":{"t":"status","session":7}}"#) {
            Ok(ServerMessage::Unknown { event, .. }) => assert_eq!(event["t"], "status"),
            other => panic!("a shape this build cannot read is kept, got {other:?}"),
        }
    }

    #[test]
    fn events_are_tagged_tightly() {
        let e = Event::Status {
            session: "main".into(),
            status: SessionStatus::Running,
            detail: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"t\":\"status\""), "{json}");
    }

    /// A head says what an item is and what it was given, and nothing about how
    /// that looks.
    ///
    /// # Invariant
    ///
    /// Which fields summarise the item travels *with each field*, so adding a tool
    /// or changing what its folded row shows is a server change: a front-end reads
    /// `summary` and never a table of names. No colour, glyph or style is on the
    /// wire -- a palette is one program's opinion about another's screen.
    #[test]
    fn a_head_says_what_an_item_is_and_what_it_was_given() {
        let e = Event::ToolAnswer(ToolAnswerEvent {
            session: "main".into(),
            call_id: "c1".into(),
            tool: "bash".into(),
            stream: ToolStream::Stdout,
            text: String::new(),
            done: true,
            head: Some(Head::new(HeadKind::Failed, "2.5s")),
        });
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""kind":"failed""#), "{json}");
        assert!(json.contains(r#""value":"2.5s""#), "{json}");
        assert!(json.contains(r#""summary":true"#), "{json}");
        for look in ["color", "colour", "fg", "symbol", "bold", "style"] {
            assert!(
                !json.contains(look),
                "{look} does not belong on the wire: {json}"
            );
        }
        // And it comes back whole: a front-end joins a result to a call by id.
        let round: Event = serde_json::from_str(&json).unwrap();
        match round {
            Event::ToolAnswer(e) => {
                assert_eq!(e.head, Some(Head::new(HeadKind::Failed, "2.5s")));
                assert_eq!(e.call_id, "c1");
            }
            other => panic!("{other:?}"),
        }
    }

    /// A call's fields are sent once, each marked with whether the folded row
    /// shows it: opening the item is local.
    #[test]
    fn a_call_sends_every_field_once_and_marks_the_ones_it_summarises() {
        let head = Head {
            kind: HeadKind::Call,
            name: "bash".into(),
            fields: vec![
                Field {
                    label: String::new(),
                    value: "cargo test".into(),
                    summary: false,
                },
                Field {
                    label: "name".into(),
                    value: "build".into(),
                    summary: true,
                },
            ],
        };
        assert_eq!(head.summary(), "bash, name=build");
        assert_eq!(head.detail(), "bash\ncargo test\nname=build");
    }

    /// A call names the tool it runs, its arguments, and the id its result carries.
    #[test]
    fn a_tool_call_is_its_own_event() {
        let e = Event::ToolCall(ToolCallEvent {
            session: "main".into(),
            call_id: "c1".into(),
            head: Head::new(HeadKind::Call, r#"bash{"command":"ls"} "#),
        });
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""t":"tool_call""#), "{json}");
        assert!(json.contains(r#""kind":"call""#), "{json}");
        let round: Event = serde_json::from_str(&json).unwrap();
        match round {
            Event::ToolCall(c) => assert_eq!(c.call_id, "c1"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn command_round_trips() {
        let msg = ClientMessage::Command {
            id: 3,
            name: "send_message".into(),
            args: Args::from([("message".to_string(), "hi".to_string())]),
        };
        let bytes = encode_client(&msg).unwrap();
        match decode_client(&bytes).unwrap() {
            ClientMessage::Command { id, name, args } => {
                assert_eq!(id, 3);
                assert_eq!(name, "send_message");
                assert_eq!(args["message"], "hi");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// A catalog entry says where it runs; saying nothing means the server.
    #[test]
    fn a_command_spec_says_who_runs_it() {
        let mut spec = CommandSpec {
            name: "echo".into(),
            description: "Say it back.".into(),
            args: vec![],
            target: CommandTarget::ServerScript,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["target"], "server_script", "{json}");
        let back: CommandSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back.target, CommandTarget::ServerScript);

        spec.target = CommandTarget::Client;
        assert_eq!(serde_json::to_value(&spec).unwrap()["target"], "client");

        // Absent means server: that is what every older entry said.
        let short: CommandSpec =
            serde_json::from_str(r#"{"name":"cancel","description":"Stop.","args":[]}"#).unwrap();
        assert_eq!(short.target, CommandTarget::Server);
    }

    /// The client-state sources name state the server cannot see.
    #[test]
    fn client_state_completion_sources_round_trip() {
        for (source, spelled) in [
            (CompletionSource::StickyArguments, "sticky_arguments"),
            (CompletionSource::Skills, "skills"),
        ] {
            let json = serde_json::to_string(&source).unwrap();
            assert_eq!(json, format!("\"{spelled}\""));
            let back: CompletionSource = serde_json::from_str(&json).unwrap();
            assert!(matches!(
                (back, source),
                (
                    CompletionSource::StickyArguments,
                    CompletionSource::StickyArguments
                ) | (CompletionSource::Skills, CompletionSource::Skills)
            ));
        }
    }

    #[test]
    fn empty_optional_fields_are_omitted() {
        let e = Event::CmdResult {
            id: 1,
            ok: true,
            message: "ok".into(),
            data: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(!json.contains("data"), "{json}");
    }
}
