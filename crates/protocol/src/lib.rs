//! Wire protocol and reliable-UDP transport shared by `gongga-server`,
//! `gongga-cli` and `gongga-tui`.
//!
//! The crate is deliberately dependency-light so the CLI does not inherit the
//! server's runtime. It contains three things:
//!
//! * [`frame`] — the fixed 26-byte datagram header.
//! * [`channel`] — a reliable, ordered, fragmented message channel over UDP.
//! * [`message`] — the JSON event vocabulary the two peers speak.

pub mod channel;
pub mod frame;
pub mod message;

pub use channel::{
    Channel, ChannelConfig, ChannelError, ChannelSink, ChannelStream, Role, accept, connect,
};
pub use message::{
    ArgSpec, Args, ClientInfo, ClientMessage, CmdArg, CommandSpec, CompletionSource, Event,
    LogLevel, ResponseEvent, ResponsePart, ServerInfo, ServerMessage, SessionStatus,
    SessionSummary, ToolAnswerEvent, ToolStream, PROTOCOL_VERSION, decode_client, decode_server,
    encode_client, encode_server,
};
