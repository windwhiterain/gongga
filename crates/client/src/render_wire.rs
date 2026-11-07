//! The serialized rendering vocabulary a client daemon speaks to its front-ends.
//!
//! See `docs/notes/client-render-wire.md`.

use serde::{Deserialize, Serialize};

use crate::{Layout, LineKind, Sink, protocol};

/// What a front-end draws: semantic kinds and text, never colours or columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RenderMessage {
    Line {
        window: String,
        kind: LineKind,
        text: String,
    },
    Stream {
        window: String,
        key: String,
        kind: LineKind,
        prefix: String,
        text: String,
        done: bool,
    },
    Status {
        text: String,
    },
    Note {
        kind: LineKind,
        text: String,
    },
    Layout {
        layout: Layout,
    },
    Rewrite {
        window: String,
        key: String,
        text: String,
    },
    ToolCall {
        window: String,
        key: String,
        head: protocol::Head,
        streamed: bool,
    },
    Relabel {
        window: String,
        key: String,
        tag: String,
        kind: LineKind,
    },
    Forget {
        window: String,
    },
    Clear {
        window: String,
    },
    StickySession {
        name: Option<String>,
    },
    Stopped {
        session: String,
        failed: bool,
    },
}

/// The cursor half of `Sink`: what a client and its window journal say to each other.
///
/// # Invariant
///
/// These are not a front-end's picture but the answers behind `declare_drawn` and
/// `ask_transcripts`. The daemon consumes them itself and never puts them on the
/// wire, so a front-end in any language stays stateless. See `docs/notes/client-render-wire.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CursorMessage {
    Settled {
        id: u64,
        ok: bool,
    },
    Transcript {
        session: String,
        created: i64,
        seq: i64,
    },
    Rewind {
        window: String,
    },
    Older {
        window: String,
    },
    Landed {
        window: String,
        created: i64,
        drawn: i64,
        floor: i64,
    },
}

/// What a front-end asks the daemon to do, in the same words a keyboard would.
///
/// # Invariant
///
/// A gesture names what it means instead of spelling a command line, so command
/// syntax stays out of the front-end. `Older` names the windows a key asked for
/// and never searches: the request is fixed when it was pressed, so the loop
/// holds the pages owed and a later frame cannot add to them behind its back.
/// See `docs/notes/tui-lib.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum UiIntent {
    /// A finished input line; it may contain newlines.
    Submit { line: String },
    /// A gesture of the window's own, never a command line.
    Operate { operation: protocol::Operation },
    /// The operator asked to read above what these windows hold.
    Older { windows: Vec<String> },
    /// Alt+Q: leave.
    Quit,
}

/// A `Sink` that encodes every call instead of drawing it.
///
/// # Invariant
///
/// `tool_call` is overridden so a structured `Head` crosses as itself; the trait's
/// default would flatten it into text and a front-end could no longer show the call.
/// See `docs/notes/client-render-wire.md`.
#[derive(Debug, Default)]
pub struct WireSink {
    render: Vec<RenderMessage>,
    cursor: Vec<CursorMessage>,
}

impl WireSink {
    pub fn take_render(&mut self) -> Vec<RenderMessage> {
        std::mem::take(&mut self.render)
    }

    pub fn take_cursor(&mut self) -> Vec<CursorMessage> {
        std::mem::take(&mut self.cursor)
    }
}

impl Sink for WireSink {
    fn line(&mut self, window: &str, kind: LineKind, text: &str) {
        self.render.push(RenderMessage::Line {
            window: window.to_string(),
            kind,
            text: text.to_string(),
        });
    }

    fn stream(
        &mut self,
        window: &str,
        key: &str,
        kind: LineKind,
        prefix: &str,
        text: &str,
        done: bool,
    ) {
        self.render.push(RenderMessage::Stream {
            window: window.to_string(),
            key: key.to_string(),
            kind,
            prefix: prefix.to_string(),
            text: text.to_string(),
            done,
        });
    }

    fn status(&mut self, text: &str) {
        self.render.push(RenderMessage::Status {
            text: text.to_string(),
        });
    }

    fn note(&mut self, kind: LineKind, text: &str) {
        self.render.push(RenderMessage::Note {
            kind,
            text: text.to_string(),
        });
    }

    fn layout(&mut self, layout: &Layout) {
        self.render.push(RenderMessage::Layout {
            layout: layout.clone(),
        });
    }

    fn rewrite(&mut self, window: &str, key: &str, text: &str) {
        self.render.push(RenderMessage::Rewrite {
            window: window.to_string(),
            key: key.to_string(),
            text: text.to_string(),
        });
    }

    fn tool_call(&mut self, window: &str, key: &str, head: &protocol::Head, streamed: bool) {
        self.render.push(RenderMessage::ToolCall {
            window: window.to_string(),
            key: key.to_string(),
            head: head.clone(),
            streamed,
        });
    }

    fn relabel(&mut self, window: &str, key: &str, tag: &str, kind: LineKind) {
        self.render.push(RenderMessage::Relabel {
            window: window.to_string(),
            key: key.to_string(),
            tag: tag.to_string(),
            kind,
        });
    }

    fn forget(&mut self, window: &str) {
        self.render.push(RenderMessage::Forget {
            window: window.to_string(),
        });
    }

    fn clear(&mut self, window: &str) {
        self.render.push(RenderMessage::Clear {
            window: window.to_string(),
        });
    }

    fn sticky_session(&mut self, name: Option<&str>) {
        self.render.push(RenderMessage::StickySession {
            name: name.map(str::to_string),
        });
    }

    fn stopped(&mut self, session: &str, failed: bool) {
        self.render.push(RenderMessage::Stopped {
            session: session.to_string(),
            failed,
        });
    }

    fn settled(&mut self, id: u64, ok: bool) {
        self.cursor.push(CursorMessage::Settled { id, ok });
    }

    fn transcript(&mut self, session: &str, created: i64, seq: i64) {
        self.cursor.push(CursorMessage::Transcript {
            session: session.to_string(),
            created,
            seq,
        });
    }

    fn rewind(&mut self, window: &str) {
        self.cursor.push(CursorMessage::Rewind {
            window: window.to_string(),
        });
    }

    fn older(&mut self, window: &str) {
        self.cursor.push(CursorMessage::Older {
            window: window.to_string(),
        });
    }

    fn landed(&mut self, window: &str, created: i64, drawn: i64, floor: i64) {
        self.cursor.push(CursorMessage::Landed {
            window: window.to_string(),
            created,
            drawn,
            floor,
        });
    }
}
