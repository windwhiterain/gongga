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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEvent {
    pub session: String,
    /// Turns are numbered from 1 so the CLI can drop stale chunks after a cancel.
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
