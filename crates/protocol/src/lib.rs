//! Wire protocol and reliable-UDP transport for the server, CLI and TUI.
//! See `docs/notes/protocol-lib.md`.

/// Which code this is: the commit, one id for all three programs.
///
/// # Invariant
///
/// The whole identity, and the only one. "Are these three one build" is then a
/// comparison rather than a guess, and a bug report carries the answer instead of
/// a release number that cannot tell two builds of that release apart.
pub const BUILD: &str = env!("GONGGA_BUILD");

/// What `--version` answers: which code this is, and nothing else.
///
/// # Invariant
///
/// `BUILD` alone. `0.1.0` is a release number that cannot tell two builds of one
/// release apart, so it is a second thing to read and a weaker answer than the
/// commit it would sit next to. One identity, one line, every program.
pub fn version_line() -> &'static str {
    BUILD
}

pub mod channel;
pub mod frame;
#[cfg(test)]
mod loopback;
pub mod message;

pub use channel::{
    Channel, ChannelConfig, ChannelError, ChannelSink, ChannelStream, MAX_MESSAGE_LEN, Role,
    accept, connect,
};
pub use message::{
    ArgSpec, Args, BLOCK_SIZES, ClientInfo, ClientMessage, CmdArg, CommandSpec, CommandTarget,
    CompletionSource, Event, LogLevel, PROTOCOL_VERSION, RENDER_MODES, REPLAY_TURN, ResponseEvent,
    ResponsePart, ServerInfo, ServerMessage, SessionInfo, SessionStatus, ToolAnswerEvent,
    ToolStream, Usage, UsageEvent, client_commands, decode_client, decode_server, encode_client,
    encode_server,
};
