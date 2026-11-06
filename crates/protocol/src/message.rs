//! JSON event vocabulary, shared by the server, the CLI and the TUI.
//!
//! See `docs/notes/protocol-message.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 3;

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
    /// An operation, beside the commands: what a window's gestures ask for.
    Operation { id: u64, operation: Operation },
    /// Polite disconnect.
    Bye,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub version: String,
    /// The build of the program dialing: see [`crate::BUILD`].
    #[serde(default)]
    pub build: String,
    /// Free-form label shown by the server in logs, e.g. `gongga-tui/pid-1234`.
    ///
    /// # Invariant
    ///
    /// The program's own name, not the crate's: this is the half of "who asked"
    /// that a log row keeps for a `cancel`, and it names a window differently from
    /// a one-shot command and from the bundle's own `gongga`.
    pub label: String,
    /// The session whose shell started this client, when one did.
    ///
    /// # Invariant
    ///
    /// Read from the caller's own environment (`$GONGGA_SESSION`), never guessed
    /// from a name the command typed: a command that names a session acts on that
    /// one, while the *caller* is the session it was run from -- and only the
    /// process knows which. `subscribe` is what needs it: it notices the session
    /// that subscribed, not the one the pattern mentions.
    #[serde(default)]
    pub session: Option<String>,
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
        /// Every session the server holds, each carrying its sync stamp.
        ///
        /// # Invariant
        ///
        /// A greeting is the one place the whole table is sent, because it is the one
        /// moment a client has none; every later change is one [`Event::Session`]. Each
        /// row's [`SessionInfo::transcript`] is what a client's own record is compared
        /// with, and a difference means a gap to fetch when that window is read.
        sessions: Vec<SessionInfo>,
        /// The models a request could run on here, and what each of them takes.
        #[serde(default)]
        models: Vec<ModelInfo>,
        /// The stream position this connection starts from: every event this
        /// server publishes afterwards is numbered above it.
        ///
        /// # Invariant
        ///
        /// The counter restarts with the process, so this number is only
        /// comparable with events of the same [`ServerInfo::pid`]; a client that
        /// redials a different server starts its stream from that one's cursor.
        #[serde(default)]
        cursor: u64,
    },
    /// The key was wrong. The connection is closed right after this.
    Denied { reason: String },
    /// One streamed observable, and where it sits in the stream.
    ///
    /// # Invariant
    ///
    /// `seq` is the stream's cursor: one counter, one step per published event, so a
    /// client that skipped one sees the jump. `None` is an event that is only this
    /// connection's answer -- a command's rows or its result -- and so has no place in
    /// the stream everybody else sees. See `docs/notes/protocol-message.md`.
    Event {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        event: Event,
    },
    /// An event whose `t` this build does not know, or whose shape it cannot read.
    ///
    /// # Invariant
    ///
    /// A new kind of event is not a breaking change: the bytes are kept as they arrived,
    /// so a reader shows an unknown kind instead of losing the connection. See
    /// `docs/notes/protocol-message.md`.
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
    /// The process this server's lifetime belongs to (`--parent`): closing it stops
    /// the server.
    #[serde(default)]
    pub owner: Option<u32>,
    /// The watchdog this server runs under, when it runs under one.
    ///
    /// # Invariant
    ///
    /// A front-end that starts a server is told the pid of the process it
    /// spawned, which is the watchdog and not the pid answering here. Without
    /// this, a window would read its own server as somebody else's, and a stop
    /// that kills the answering process alone would be undone by a restart.
    #[serde(default)]
    pub supervisor: Option<u32>,
    /// Whether this server is finishing a `cancel`: it answers for a short
    /// notice window and starts no new turn.
    ///
    /// # Invariant
    ///
    /// Stated so a window can tell "the server is stopping" from "the server is
    /// gone". Both look like silence on a wire, and a reader who has to guess
    /// guesses wrong -- a slow cancel looked exactly like a crash.
    #[serde(default)]
    pub draining: bool,
}

/// One event and its place in the stream: what the bus hands a subscriber.
///
/// # Invariant
///
/// The number is assigned where the event is published and never changes; it is
/// the same number [`ServerMessage::Event`] carries, so a subscriber that keeps
/// this and a client that keeps that are keeping one cursor.
#[derive(Debug, Clone)]
pub struct Delta {
    pub seq: u64,
    pub event: Event,
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
    /// One session was created or changed: its row, as the server states it.
    ///
    /// # Invariant
    ///
    /// One row, never the list. A client holds the sessions it was greeted with
    /// and applies these; the whole table is republished only by a greeting,
    /// which is the one moment a client is allowed to have none.
    Session { session: SessionInfo },
    /// A session is gone: a window named after it is emptied.
    SessionGone { name: String },
    /// Where one session's transcript ends: every row up to `seq` is written down.
    ///
    /// # Invariant
    ///
    /// The watermark a client keeps, not a fact about a window: one that drew every
    /// event up to this has drawn every row up to `seq`. It can go backwards -- a
    /// failed step's rows are truncated -- so "different" is the comparison, not
    /// "behind".
    Transcript { session: String, seq: i64 },
    /// The models a request could run on here changed.
    ///
    /// # Invariant
    ///
    /// The whole list, never a delta: the roster is one file a person edits, and
    /// a reader that applied differences would have to know what it missed while
    /// it was away. What each model *takes* is not here: it is `ArgSpec.accepts`
    /// of the argument that carries it, republished by [`Self::Commands`].
    Models { models: Vec<ModelInfo> },
    /// The command catalog, re-published.
    ///
    /// # Invariant
    ///
    /// A catalog is a snapshot of the roster, because an argument publishes the
    /// values it accepts. Editing `providers.toml` therefore makes the greeting's
    /// catalog stale, and this message is the whole of it again -- never a delta,
    /// for the same reason [`Self::Models`] is whole. A client that never sees one
    /// keeps the catalog it was greeted with.
    Commands { commands: Vec<CommandSpec> },
    /// A session's run state changed.
    ///
    /// # Invariant
    ///
    /// `outcome` is only there when a turn ended, and it is a word rather than a
    /// sentence: a reader deciding whether a stop is worth telling anyone about
    /// must not parse `detail`. `detail` stays the sentence a person reads.
    Status {
        session: String,
        status: SessionStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<TurnOutcome>,
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
    /// The shape is `kind` plus a free-form `payload`, never a variant per kind: the log
    /// is append-only data, so a closed enum would mean a wire change for every new kind.
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
            Event::Session { session } => Some(session.name.as_str()),
            Event::SessionGone { name } => Some(name.as_str()),
            Event::Transcript { session, .. } => Some(session.as_str()),
            Event::Cmd { .. }
            | Event::CmdResult { .. }
            | Event::Models { .. }
            | Event::Commands { .. } => None,
            Event::Log { .. } | Event::Quota(_) => None,
        }
    }

    /// The name a `subscribe <name> <pattern>` pattern is matched against.
    ///
    /// # Invariant
    ///
    /// A log row is named by its own `kind`, every other event by its variant:
    /// that is the whole vocabulary a pattern can spell. It is not a second
    /// dispatch table -- a new row kind needs no entry here, which is the same
    /// reason [`Event::Record`] carries its kind as data.
    pub fn name(&self) -> &str {
        match self {
            Event::Record { kind, .. } => kind.as_str(),
            Event::Cmd { .. } => "cmd",
            Event::CmdResult { .. } => "cmd_result",
            Event::Response(_) => "response",
            Event::ToolCall(_) => "tool_call",
            Event::ToolAnswer(_) => "tool_answer",
            Event::Session { .. } => "session",
            Event::SessionGone { .. } => "session_gone",
            Event::Transcript { .. } => "transcript",
            Event::Models { .. } => "models",
            Event::Commands { .. } => "commands",
            Event::Status { .. } => "status",
            Event::Log { .. } => "log",
            Event::Usage(_) => "usage",
            Event::Quota(_) => "quota",
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
    /// The operator's own words, replayed from the transcript.
    ///
    /// # Invariant
    ///
    /// Never live: a message this client sent was drawn from its own echo, and
    /// the reply's chunks are the model's. This part exists so a fill can put a
    /// message back in the window exactly as the echo that wrote it did -- the
    /// row is a row like any other, and a window that drops it drops the half of
    /// the conversation it was answering.
    Input,
}

/// The turn a read re-sends a stored row under, which no live turn can use.
///
/// # Invariant
///
/// Turns count from 1, so `0` tells a front-end this is a row asked for by name:
/// the block it opens stands beside a live stream, never inside it.
pub const REPLAY_TURN: u64 = 0;

/// The log row a `cancel` writes before it stops anything, naming who asked.
///
/// # Invariant
///
/// The row is the record and its publication the notice: a window that did not
/// ask learns the server is stopping from this row, and reads who asked from
/// `payload.by`. Nothing else carries that answer -- the asker's own result goes
/// to the asker's connection, and the status line states the fact, not the name.
/// See `docs/notes/server-commands.md`.
pub const SERVER_STOP: &str = "server_stop";

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
    /// What the item is for, in the words it was asked in; empty when it says nothing.
    ///
    /// # Invariant
    ///
    /// A call carries at most one, and it is the model's own sentence: it is read
    /// on the name's row, right after the name, which is the only place a folded
    /// item can afford one line of prose. It is not a [`Field`]: an argument with
    /// no label of its own would be spelled a second time under a name the row
    /// already said.
    #[serde(default)]
    pub description: String,
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
            description: String::new(),
            fields: vec![Field {
                label: String::new(),
                value: text.into(),
                summary: true,
            }],
        }
    }

    /// A tool call: its name, what it is for, then the arguments the model gave it.
    pub fn call(
        name: impl Into<String>,
        description: impl Into<String>,
        fields: Vec<Field>,
    ) -> Self {
        Self {
            kind: HeadKind::Call,
            name: name.into(),
            description: description.into(),
            fields,
        }
    }

    /// The name and, after it, what the item is for: one row's first words.
    ///
    /// # Invariant
    ///
    /// The separator is spelled once, here, because both rows of an item open with
    /// this and a front-end that had to join them would be a second place to change.
    /// An item with no description is its name alone -- never a trailing mark.
    fn title(&self) -> String {
        match self.description.is_empty() {
            true => self.name.clone(),
            false => format!("{}\u{b7}{}", self.name, self.description),
        }
    }

    /// The folded row: SPEC's `abstract`, from the title and the marked fields.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.name.is_empty() {
            parts.push(self.title());
        }
        parts.extend(self.fields.iter().filter(|f| f.summary).map(spelled));
        parts.join(", ")
    }

    /// The open item: the title, then one row per argument.
    pub fn detail(&self) -> String {
        let mut rows: Vec<String> = Vec::new();
        if !self.name.is_empty() {
            rows.push(self.title());
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

/// How a turn ended: what the status alone cannot say.
///
/// # Invariant
///
/// `Done` and `Failed` are a turn the agent finished, and `Cancelled` is a turn
/// somebody stopped -- a `cancel`, an `interrupt_session`, or the window that
/// owned the server leaving. A front-end tells the operator about the first two
/// and says nothing about the third: a stop it asked for needs no announcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Done,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub status: SessionStatus,
    pub created_at: i64,
    pub updated_at: i64,
    /// When its last turn ended, in the server's own milliseconds; `None` is a
    /// session that has not finished one.
    ///
    /// # Invariant
    ///
    /// This is the key the session list is ordered by, and it is published so a
    /// client can put a changed row back where it belongs without asking for the
    /// list again. A session that never finished a turn stands at `created_at`,
    /// which every client already has.
    #[serde(default)]
    pub ended_at: Option<i64>,
    pub messages: u64,
    /// The seq of its last stored row, or 0 for an empty transcript.
    ///
    /// # Invariant
    ///
    /// The sync stamp. A client that drew every row up to this number is in sync
    /// with this session; one that is behind it fetches the rest lazily. It is
    /// the same number `Event::Transcript` publishes, read from the same table,
    /// so a reader that compares them is comparing one fact with itself.
    #[serde(default)]
    pub transcript: i64,
    /// Its own shell's directory: a value completed for it is read from where a
    /// tool reads. Never the server's.
    pub cwd: String,
    /// The last measurement, so a window attached now draws it without a turn.
    #[serde(default)]
    pub usage: Usage,
    /// The model a turn here would spend on, `provider/model_name`; `None` is a
    /// server that does not know what it would send.
    #[serde(default)]
    pub model: Option<String>,
    /// Reasoning level in force, in the roster's spelling; `None` is a model that
    /// publishes no level.
    #[serde(default)]
    pub thinking_effort: Option<String>,
    /// The share of the window a context notice is sent at, when this session has one.
    #[serde(default)]
    pub context_limit: Option<u8>,
    /// The remote this session's shells run on; `None` is this machine.
    ///
    /// # Invariant
    ///
    /// Published so a front-end can complete a value that belongs to that machine
    /// without the operator repeating `--remote`: the session already says it.
    #[serde(default)]
    pub remote: Option<String>,
}

/// One model a request could run on here.
///
/// # Invariant
///
/// A name and nothing else. What a model *takes* -- its reasoning levels, its
/// cache tiers -- is published by the argument that takes it, as
/// [`ArgSpec::accepts`], because that is the value the server would accept and
/// the roster's table is only how it works it out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// `provider/model_name`, as `--model` spells it.
    pub model: String,
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

/// An operation a client asks for: the gestures a window offers, beside the
/// commands it types.
///
/// # Invariant
///
/// These are API, not commands: a window asks for "make this session sticky",
/// never for a command line to spell. The client answers the two that are its
/// own state, and the server answers the one that is not -- and both answer
/// them the same way the matching command does, so there is one behavior per
/// gesture. See `docs/notes/protocol-message.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// Make a session the sticky one.
    StickySession { session: String },
    /// Show or hide a session's window.
    ///
    /// # Invariant
    ///
    /// `solo` is the plain click on a name: the session becomes the one shown and
    /// every other window closes, the way a list selects. `false` touches only
    /// this session's window, which is what Shift+click and the typed command do.
    RenderSession {
        session: String,
        show: bool,
        #[serde(default)]
        solo: bool,
    },
    /// Delete a session, its shells and its transcript.
    DeleteSession { session: String },
}

impl Operation {
    /// The command an operation asks the server for, when it asks for one.
    ///
    /// # Invariant
    ///
    /// `None` for the two that are the client's own state: the server cannot
    /// change a caller's window, and saying "unknown" would hide a request
    /// that is right there.
    pub fn as_command(&self) -> Option<(&'static str, Args)> {
        match self {
            Operation::DeleteSession { session } => {
                let mut args = Args::new();
                // The spec's binding name (its flag is --name); what a sticky
                // entry keys on is the kind, which is `session`.
                args.insert("name".to_string(), session.clone());
                Some(("del_session", args))
            }
            Operation::StickySession { .. } | Operation::RenderSession { .. } => None,
        }
    }
}

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

/// SPEC's starting margin: the columns a box leaves to its pane on each side.
pub const DEFAULT_BOX_EDGE: usize = 2;

/// The widths `set_render --box_edge` offers: how much of a window's width each
/// side of a box gives back to the pane.
pub const BOX_EDGES: &[&str] = &["2", "0", "1", "3"];

/// The values `set_render_session --show` takes.
pub const SHOW_VALUES: &[&str] = &["true", "false"];

/// The value that names none of something, where omitting would inherit one.
///
/// # Invariant
///
/// One spelling for every argument that has an "off": `--remote none` is this
/// machine, `--cache_tier none` is no tier at all. Absence means something else
/// everywhere -- the session keeps what it had -- so "none" is a statement and
/// never a default.
pub const NONE_VALUE: &str = "none";

/// The `--thinking` value that names no level: the level a model uses by itself.
///
/// # Invariant
///
/// One spelling: the completion offers it and `set_session` reads it, and no
/// roster publishes it as a level of its own.
pub const THINKING_DEFAULT: &str = "default";

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
            "Set one sticky entry, applied to every later command that declares its type.",
            vec![
                spec_arg(
                    "type",
                    "Argument type -- the kind a published argument declares, e.g. \
                     `session`. The catalog's kinds are the only entries there are.",
                    true,
                    true,
                    None,
                    Some("name"),
                    CompletionSource::Arguments,
                ),
                spec_arg(
                    "value",
                    "Value to apply when the argument is omitted. Completes the way an \
                     argument of this type does.",
                    true,
                    true,
                    None,
                    Some("value"),
                    CompletionSource::OfNamedArgument,
                ),
            ],
        ),
        (
            "del_sticky",
            "Remove a sticky argument.",
            vec![spec_arg(
                "type",
                "Argument type to remove, e.g. `session`.",
                true,
                true,
                None,
                Some("name"),
                CompletionSource::StickyArguments,
            )],
        ),
        (
            "set_render",
            "How the events are drawn; with no argument, the settings it has.",
            vec![
                spec_arg(
                    "block_size",
                    "`0` caps nothing. Otherwise a non-answer block shows at most this \
                     many wrapped rows and keeps the newest of them, so a long stream \
                     scrolls inside its own block instead of pushing other blocks away.",
                    false,
                    false,
                    None,
                    Some("lines"),
                    CompletionSource::Static(BLOCK_SIZES.iter().map(|m| m.to_string()).collect()),
                ),
                spec_arg(
                    "box_edge",
                    "Columns an opened item leaves to its window on the left and on the \
                     right: a wheel over that margin scrolls the window, and one over the \
                     box scrolls the box. `0` draws the box edge to edge.",
                    false,
                    false,
                    None,
                    Some("columns"),
                    CompletionSource::Static(BOX_EDGES.iter().map(|m| m.to_string()).collect()),
                ),
            ],
        ),
        (
            "set_render_session",
            "Whether one session is shown beside the list.",
            vec![
                spec_arg(
                    "show",
                    "`true` gives the session a header and its events beside the list; \
                     `false` leaves it as a name in the list.",
                    true,
                    true,
                    None,
                    Some("true|false"),
                    CompletionSource::Static(SHOW_VALUES.iter().map(|m| m.to_string()).collect()),
                ),
                spec_arg(
                    "session",
                    "The session whose window this shows or hides.",
                    true,
                    false,
                    Some("session"),
                    Some("name"),
                    CompletionSource::Sessions,
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
                    false,
                    None,
                    Some("ip"),
                    CompletionSource::None,
                ),
                spec_arg(
                    "port",
                    "The UDP port it listens on.",
                    true,
                    false,
                    None,
                    Some("port"),
                    CompletionSource::None,
                ),
            ],
        ),
        (
            "serve",
            "Run a server for this home in this process, in the foreground.",
            vec![
                spec_arg(
                    "port",
                    "The UDP port to listen on, on localhost.",
                    true,
                    true,
                    None,
                    Some("port"),
                    CompletionSource::None,
                ),
                spec_arg(
                    "home",
                    "The harness home to serve; this client's own home when omitted.",
                    false,
                    false,
                    None,
                    Some("path"),
                    CompletionSource::Paths,
                ),
            ],
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
    positional: bool,
    kind: Option<&str>,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> ArgSpec {
    ArgSpec {
        name: name.into(),
        description: description.into(),
        required,
        positional,
        kind: kind.map(str::to_string),
        value_hint: value_hint.map(str::to_string),
        completions,
        accepts: None,
        sticky_to: None,
        sticky: false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgSpec {
    /// Name without leading dashes, e.g. `message`. The binding key on the wire.
    ///
    /// # Invariant
    ///
    /// Free to change, and it does: `set_session --name <session>` and
    /// `get_messages --session <session>` bind different names to one idea.
    /// What a sticky entry keys on is `kind`, never this.
    pub name: String,
    pub description: String,
    pub required: bool,
    /// Bound by position, in the order published, not by a `--flag`.
    ///
    /// # Invariant
    ///
    /// SPEC's command lines spell these `<name>`; the flag arguments stay `--name`.
    /// The declaration order in `args` is the binding order, so a spec's list is
    /// its syntax. Every non-optional argument may also be spelled by order alone
    /// (SPEC: "all non optional named argument's name can be ommited via argument
    /// order"); an optional one needs its flag.
    #[serde(default)]
    pub positional: bool,
    /// Shown as the value placeholder during completion.
    pub value_hint: Option<String>,
    /// The argument's type: the sticky entry that fills it.
    ///
    /// # Invariant
    ///
    /// SPEC's `<session>`, `<model>` and so on. Two arguments of one kind share
    /// one sticky entry wherever they appear, whatever their names spell; the
    /// kinds the published catalog declares are the only sticky entries there
    /// are, and an argument with no kind never fills from `Sticky`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Where the CLI should source completion candidates.
    ///
    /// # Invariant
    ///
    /// This is what is *worth offering*, never what is allowed: `--ctx_limit`
    /// offers ten percentages and accepts any of a hundred. When the set is
    /// closed, [`Self::accepts`] says so and this is [`CompletionSource::None`],
    /// so there is one list per argument and not two that can disagree.
    #[serde(default)]
    pub completions: CompletionSource,
    /// The values this argument accepts, when the set is closed; `None` when any
    /// value is accepted.
    ///
    /// # Invariant
    ///
    /// Exactly what the command will not refuse -- the server's own validation is
    /// the source of these values, so a client can answer "is this value taken?"
    /// from the published catalog alone, and no front-end derives it from the
    /// roster a second time.
    #[serde(default)]
    pub accepts: Option<Accepts>,
    /// SPEC's `(sticky to <kind>)`: this argument's entry is forgotten when that
    /// one changes.
    ///
    /// # Invariant
    ///
    /// The dependency is data, published with the argument: a value that belongs
    /// to a model or a machine is not carried to the next one, and the reader that
    /// keeps the entries applies the rule without knowing a single name. A
    /// follower of a kind that never changes is never forgotten.
    #[serde(default)]
    pub sticky_to: Option<String>,
    /// SPEC's `(sticky)`: filled from the entry when omitted, and written back
    /// to it when the command succeeds.
    ///
    /// # Invariant
    ///
    /// A required argument fills whether or not this is set; the flag exists
    /// for the optional ones a reader thinks of as a mode rather than a fact
    /// about one command. Absence is still a decision everywhere else: a filter
    /// that keeps what it had must not be overwritten by an entry meant for a
    /// different command.
    #[serde(default)]
    pub sticky: bool,
}

impl ArgSpec {
    /// Names the entry this one follows: SPEC's `(sticky to <kind>)`.
    pub fn following(mut self, kind: &str) -> Self {
        self.sticky_to = Some(kind.to_string());
        self
    }

    /// Narrows what the argument accepts, and leaves offering to that same list.
    ///
    /// # Invariant
    ///
    /// One list per argument: a closed set is published as `accepts`, and the
    /// completion source it would otherwise have had becomes `None`, so no two
    /// lists of one argument can disagree about a value.
    pub fn accepting(mut self, accepts: Accepts) -> Self {
        self.accepts = Some(accepts);
        self.completions = CompletionSource::None;
        self
    }

    /// The same, from a read that may state nothing.
    ///
    /// # Invariant
    ///
    /// A read with no answer leaves the argument unconstrained: "nothing states a
    /// set" must not be published as "the set is empty".
    pub fn accepting_when(mut self, read: impl FnOnce() -> Option<Accepts>) -> Self {
        match read() {
            Some(accepts) => {
                self.accepts = Some(accepts);
                self.completions = CompletionSource::None;
            }
            None => {}
        }
        self
    }
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
    /// Complete against a directory of the session the line names.
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
    /// Complete against the models this server could send a request on.
    Models,
    /// Complete against the skills the line's session can open.
    ///
    /// # Invariant
    ///
    /// The names are read where the server reads them -- the session's own
    /// `.agents/skills`, the home's `skills/` and the machine's -- so what a
    /// reader completes is what can be opened there.
    Skills,
}

/// The values an argument accepts, when the set is closed.
///
/// # Invariant
///
/// One reading, two shapes: a value is accepted when it is in one of these lists.
/// An argument with no [`Accepts`] accepts any value -- the absence is "the
/// command looks at nothing", never "it accepts nothing", which is stated by an
/// empty list instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accepts {
    /// The same values whatever the line runs on.
    Same(Vec<String>),
    /// By model, because the roster says something different for each.
    ///
    /// # Invariant
    ///
    /// Keyed by `provider/model_name`. An absent model accepts any value -- the
    /// roster has no opinion, which is not the same as an empty list, which takes
    /// none. The model a line resolves to is the one it names, else its session's,
    /// else the server's: what the command would really run on.
    PerModel(BTreeMap<String, Vec<String>>),
}

impl Accepts {
    /// Every value any model would accept, for the case where the line names no
    /// model at all.
    pub fn union(&self) -> Vec<String> {
        match self {
            Accepts::Same(values) => values.clone(),
            Accepts::PerModel(table) => {
                let mut seen: Vec<String> = Vec::new();
                for value in table.values().flatten() {
                    if !seen.contains(value) {
                        seen.push(value.clone());
                    }
                }
                seen
            }
        }
    }

    /// Whether the value is one this accepts, for a line resolving to `model`.
    ///
    /// # Invariant
    ///
    /// `None` means the line named no model and none could be worked out, so the
    /// question cannot be answered: a value is not wrong because the reader could
    /// not tell which model would judge it.
    pub fn takes(&self, model: Option<&str>, value: &str) -> Option<bool> {
        match self {
            Accepts::Same(values) => Some(values.iter().any(|known| known == value)),
            Accepts::PerModel(table) => {
                let model = model?;
                Some(
                    table
                        .get(model)
                        .map(|values| values.iter().any(|known| known == value))
                        .unwrap_or(true),
                )
            }
        }
    }
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
/// Only a message that *is* an event falls back: a corrupt frame stays an error, or a bug
/// becomes a silent gap. A known tag with unreadable fields falls back too.
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
