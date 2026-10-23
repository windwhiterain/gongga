//! The IM side of the bridge.
//!
//! Two traits instead of one struct, mirroring how `protocol` splits a
//! connection into a sink and a stream: an inbound half that owns the socket it
//! reads events from, and an outbound half that is just an API client and can
//! be called from anywhere. Keeping them separate is not cosmetic -- a single
//! `&mut self` type with both `recv` and `send` cannot be driven from one
//! `tokio::select!`, because both futures would borrow it at once.
//!
//! Everything platform-specific (tokens, websocket framing, how a reply is
//! attached to its source message, length limits) lives behind these traits, so
//! `main.rs` and `chat.rs` never learn which IM is on the other end.

use anyhow::Result;
use async_trait::async_trait;

pub mod qq;

/// Which conversation a message belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChatId {
    /// A group chat, addressed by the platform's group id.
    Group(String),
    /// A one-to-one conversation.
    Direct(String),
}

impl ChatId {
    /// The gongga session this conversation maps to.
    ///
    /// Deterministic on purpose: there is no mapping table to lose, and a
    /// restart picks every conversation up exactly where it left off. The
    /// platform id is already unique, so the derived name is too.
    pub fn session_name(&self, kind: &str) -> String {
        match self {
            ChatId::Group(id) => format!("{kind}-g-{id}"),
            ChatId::Direct(id) => format!("{kind}-d-{id}"),
        }
    }

    /// Short form for logs: a full openid is 32 characters of noise.
    pub fn label(&self) -> String {
        let (tag, id) = match self {
            ChatId::Group(id) => ("g", id),
            ChatId::Direct(id) => ("d", id),
        };
        let short: String = id.chars().take(8).collect();
        format!("{tag}:{short}")
    }
}

/// A message as the platform delivered it.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub chat: ChatId,
    /// Platform id of the sender, for the allow-list.
    pub user: String,
    /// Body with mentions stripped, ready to become a `send_message`.
    pub text: String,
    /// The message being answered. Some platforms refuse a reply that does not
    /// reference it (QQ: five minutes, five replies), so it travels with the
    /// message rather than being looked up later.
    pub reply: MsgRef,
}

/// A reference to one delivered message.
#[derive(Debug, Clone)]
pub struct MsgRef {
    pub id: String,
}

/// A message to deliver.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub chat: ChatId,
    pub text: String,
    /// `Some` answers a specific message; `seq` counts the attempts on it,
    /// because platforms cap how many replies one message may carry (QQ: 5).
    /// `None` starts a conversation of its own.
    pub reply_to: Option<(MsgRef, u32)>,
}

#[async_trait]
pub trait Inbox: Send {
    /// Short platform name, used as the session-name prefix (`qq-g-...`).
    fn kind(&self) -> &'static str;
    /// Next message, blocking until there is one. Implementations reconnect
    /// internally; an error means "retry after a pause", not "give up".
    async fn recv(&mut self) -> Result<Incoming>;
}

#[async_trait]
pub trait Outbox: Send + Sync {
    async fn send(&self, out: Outgoing) -> Result<()>;
}

/// What a channel hands the bridge: one reader, one writer.
pub struct Channels {
    pub inbox: Box<dyn Inbox>,
    pub outbox: std::sync::Arc<dyn Outbox>,
}

impl Channels {
    pub fn kind(&self) -> &'static str {
        self.inbox.kind()
    }
}
