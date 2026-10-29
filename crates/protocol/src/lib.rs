//! Wire protocol and reliable-UDP transport for the server, CLI and TUI.
//! See `docs/notes/protocol-lib.md`.

pub mod channel;
pub mod frame;
pub mod message;

pub use channel::{
    Channel, ChannelConfig, ChannelError, ChannelSink, ChannelStream, MAX_MESSAGE_LEN, Role,
    accept, connect,
};
pub use message::{
    ArgSpec, Args, BLOCK_SIZES, ClientInfo, ClientMessage, CmdArg, CommandSpec, CommandTarget,
    CompletionSource, EVENT_FIELDS, Event, LogLevel, PROTOCOL_VERSION, ResponseEvent, ResponsePart,
    SPLIT_VALUES, ServerInfo, ServerMessage, SessionStatus, SessionSummary, ToolAnswerEvent,
    ToolStream, client_commands, decode_client, decode_server, encode_client, encode_server,
};
