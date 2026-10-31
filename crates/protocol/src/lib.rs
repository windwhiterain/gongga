//! Wire protocol and reliable-UDP transport for the server, CLI and TUI.
//! See `docs/notes/protocol-lib.md`.

/// Which build this is: one id for the server, the command line and the window.
///
/// # Invariant
///
/// Every program that links this crate carries the same string, so "are these
/// three one build" is a comparison and not a guess. It is not the version:
/// `0.1.0` says which release, this says which commit -- and two builds of one
/// release are exactly the case a version cannot answer.
pub const BUILD: &str = env!("GONGGA_BUILD");

/// What `--version` answers: the release, then the build in `BUILD`'s spelling.
///
/// # Invariant
///
/// The build is part of the answer because `0.1.0` alone cannot tell two builds
/// apart, and "same version" is the question a person asks when something acts
/// strangely. One line, greppable, identical in all three programs.
pub fn version_line() -> &'static str {
    static LINE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LINE.get_or_init(|| format!("{} ({BUILD})", env!("CARGO_PKG_VERSION")))
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
    ToolStream, client_commands, decode_client, decode_server, encode_client, encode_server,
};
