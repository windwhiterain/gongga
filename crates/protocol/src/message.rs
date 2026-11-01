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
    /// The row's words, already spelled: a front-end never formats a duration.
    #[serde(default)]
    pub text: String,
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

impl Head {
    pub fn new(kind: HeadKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
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
    /// The call as it is read: the tool, the arguments it was given.
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

pub fn decode_server(bytes: &[u8]) -> Result<ServerMessage, serde_json::Error> {
    serde_json::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A head carries a kind and words, and nothing about how they look.
    ///
    /// # Invariant
    ///
    /// The wire shape is what keeps restyling off it: a colour or a glyph here
    /// would be one program with opinions about another's palette.
    #[test]
    fn a_head_is_a_kind_and_its_words_only() {
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
        assert!(json.contains(r#""text":"2.5s""#), "{json}");
        for look in ["color", "colour", "fg", "symbol", "bold"] {
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
