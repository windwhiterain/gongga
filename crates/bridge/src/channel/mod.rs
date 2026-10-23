//! The IM side: an inbound socket reader and an outbound HTTP client.
//! See `docs/notes/bridge-channel-mod.md`.

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
    /// The session this conversation maps to; derived, so a restart resumes it.
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
    /// The message being answered: some platforms refuse a reply without it.
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
    /// `Some` answers that message, `seq` counting attempts; `None` starts fresh.
    pub reply_to: Option<(MsgRef, u32)>,
}

#[async_trait]
pub trait Inbox: Send {
    /// Short platform name, used as the session-name prefix (`qq-g-...`).
    fn kind(&self) -> &'static str;
    /// Next message, blocking. An error means "retry after a pause", not "give up".
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
