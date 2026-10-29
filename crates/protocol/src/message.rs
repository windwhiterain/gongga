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
    /// Free-form label shown by the server in logs, e.g. `cli/pid-1234`.
    pub label: String,
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
        sessions: Vec<SessionSummary>,
    },
    /// The key was wrong. The connection is closed right after this.
    Denied { reason: String },
    /// A single streamed observable.
    Event(Event),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub version: String,
    pub pid: u32,
    pub started_at: i64,
    pub cwd: String,
    pub model: String,
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
    /// Streamed tool output (bash stdout/stderr).
    ToolAnswer(ToolAnswerEvent),
    /// The set of sessions changed.
    Sessions { sessions: Vec<SessionSummary> },
    /// A session's run state changed.
    Status {
        session: String,
        status: SessionStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Diagnostic line for the TUI status bar.
    Log { level: LogLevel, message: String },
}

/// Which sub-stream of an assistant response a chunk belongs to.
/// See `docs/notes/protocol-message.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAnswerEvent {
    pub session: String,
    pub call_id: String,
    pub tool: String,
    pub stream: ToolStream,
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub name: String,
    pub status: SessionStatus,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: u64,
    pub cwd: String,
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
    pub summary: String,
    pub args: Vec<ArgSpec>,
    /// Who runs it. Published by the server, so no front-end keeps a list.
    #[serde(default)]
    pub target: CommandTarget,
}

/// The event fields an event filter's `--name` may select.
///
/// # Invariant
///
/// It lives here, not in a front-end: the server publishes `set_event_filter`,
/// so the list is catalog data.
pub const EVENT_FIELDS: &[&str] = &[
    "args", "call_id", "data", "detail", "done", "id", "level", "message", "name", "ok", "part",
    "session", "sessions", "status", "stream", "t", "text", "tool", "turn",
];

/// The lines `set_event_ui --block_size` offers, in the order it lists them.
///
/// # Invariant
///
/// Any number is accepted; this is the set worth offering, not the set allowed.
pub const BLOCK_SIZES: &[&str] = &["0", "1", "2", "3", "5"];

/// The values `set_event_ui --split` takes.
pub const SPLIT_VALUES: &[&str] = &["true", "false"];

/// The word `set_event_ui --session` takes to mean "show every session".
///
/// # Invariant
///
/// It is not a session name: naming a session sets the filter, naming this clears
/// it, so one argument carries both directions.
pub const NO_SESSION: &str = "none";

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
            "set_sticky_cmd_argument",
            "Set a sticky argument applied to every later command.",
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
            "del_sticky_cmd_argument",
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
            "set_event_filter",
            "Show only events whose named field has this value.",
            vec![
                spec_arg(
                    "name",
                    "Event field, e.g. `session`.",
                    true,
                    Some("name"),
                    CompletionSource::Static(EVENT_FIELDS.iter().map(|f| f.to_string()).collect()),
                ),
                spec_arg(
                    "value",
                    "Value the field must have. Completes the way the named argument does.",
                    true,
                    Some("value"),
                    CompletionSource::OfNamedArgument,
                ),
            ],
        ),
        (
            "del_event_filter",
            "Remove an event filter.",
            vec![spec_arg(
                "name",
                "Filter name to remove.",
                true,
                Some("name"),
                CompletionSource::EventFilters,
            )],
        ),
        (
            "set_event_ui",
            "How the events area is drawn; with no argument, all three settings.",
            vec![
                spec_arg(
                    "block_size",
                    "`0` caps nothing. Otherwise a non-answer block shows at most this many \
                     wrapped rows and keeps the newest of them, so a long stream scrolls \
                     inside its own block instead of pushing other blocks away.",
                    false,
                    Some("lines"),
                    CompletionSource::Static(BLOCK_SIZES.iter().map(|m| m.to_string()).collect()),
                ),
                spec_arg(
                    "session",
                    "Render only this session's events; `none` renders every session's. \
                     Nothing is dropped on the way in: an unseen line is a line not drawn, \
                     not a line not received.",
                    false,
                    Some("name|none"),
                    CompletionSource::SessionsOrNone,
                ),
                spec_arg(
                    "split",
                    "`true` gives each session its own window and puts `cmd`/`cmd_result` on \
                     a line of their own; `false` keeps one window for everything.",
                    false,
                    Some("bool"),
                    CompletionSource::Static(SPLIT_VALUES.iter().map(|v| v.to_string()).collect()),
                ),
            ],
        ),
    ]
    .into_iter()
    .map(|(name, summary, args)| CommandSpec {
        name: name.into(),
        summary: summary.into(),
        args,
        target: CommandTarget::Client,
    })
    .collect()
}

/// One argument of a published command.
fn spec_arg(
    name: &str,
    summary: &str,
    required: bool,
    value_hint: Option<&str>,
    completions: CompletionSource,
) -> ArgSpec {
    ArgSpec {
        name: name.into(),
        summary: summary.into(),
        required,
        value_hint: value_hint.map(str::to_string),
        completions,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgSpec {
    /// Name without leading dashes, e.g. `message`.
    pub name: String,
    pub summary: String,
    pub required: bool,
    /// Shown as the value placeholder during completion.
    pub value_hint: Option<String>,
    /// Where the CLI should source completion candidates.
    #[serde(default)]
    pub completions: CompletionSource,
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
    /// Complete against the client's event-filter names, same shape.
    EventFilters,
    /// Complete against the live session list plus `none`, which clears a filter.
    /// See `docs/notes/protocol-message.md`.
    SessionsOrNone,
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

pub fn decode_server(bytes: &[u8]) -> Result<ServerMessage, serde_json::Error> {
    serde_json::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_tagged_compactly() {
        let e = Event::Status {
            session: "main".into(),
            status: SessionStatus::Running,
            detail: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"t\":\"status\""), "{json}");
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
            summary: "Say it back.".into(),
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
            serde_json::from_str(r#"{"name":"cancel","summary":"Stop.","args":[]}"#).unwrap();
        assert_eq!(short.target, CommandTarget::Server);
    }

    /// The client-state sources name state the server cannot see.
    #[test]
    fn client_state_completion_sources_round_trip() {
        for (source, spelled) in [
            (CompletionSource::StickyArguments, "sticky_arguments"),
            (CompletionSource::EventFilters, "event_filters"),
        ] {
            let json = serde_json::to_string(&source).unwrap();
            assert_eq!(json, format!("\"{spelled}\""));
            let back: CompletionSource = serde_json::from_str(&json).unwrap();
            assert!(matches!(
                (back, source),
                (
                    CompletionSource::StickyArguments,
                    CompletionSource::StickyArguments
                ) | (
                    CompletionSource::EventFilters,
                    CompletionSource::EventFilters
                )
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
