//! The one line per frame both ends of a runner connection speak.
//!
//! See `docs/notes/server-remote.md`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The version both ends must agree on before any other frame counts.
///
/// # Invariant
///
/// Raised when a request gains a meaning an older runner would silently ignore: a
/// runner that dropped `args` would run the program it was given with a shell's
/// flags, which is a wrong answer rather than a missing one. 2 added `args` to
/// `Open`, which is how a program other than a shell is started.
pub const PROTOCOL: u32 = 2;

/// One request; `id` is the client's, and the reply repeats it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

impl Request {
    pub fn new(id: u64, op: Op) -> Self {
        Self { id, op }
    }
}

/// What a request asks for. Every operation answers on its own, so a busy shell
/// never holds one up.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Start a program under the client's own id, rooted at an endpoint-local `cwd`.
    Open {
        shell: u64,
        cwd: Option<String>,
        /// The program; absent is the peer's `bash`.
        program: Option<String>,
        /// Its arguments. Absent means a shell's own, which no other program may get.
        #[serde(default)]
        args: Option<Vec<String>>,
    },
    /// Bytes for that shell's stdin, base64 because a line has no binary.
    Write {
        shell: u64,
        data: String,
    },
    /// A signal for the shell's whole process group.
    Signal {
        shell: u64,
        signal: String,
    },
    /// Stop sending this shell's output: its client is behind. Not a cancel.
    Pause {
        shell: u64,
    },
    /// Send this shell's output again; the client has caught up.
    Resume {
        shell: u64,
    },
    /// End the shell and report whether it is confirmed gone.
    Close {
        shell: u64,
    },
    Stat {
        path: String,
    },
    Read {
        path: String,
        offset: u64,
    },
    List {
        path: String,
    },
    /// The room left on this machine's volumes, read from `path`'s point of view.
    Space {
        path: String,
    },
}

impl Op {
    /// What this request asks, as the wire spells it.
    ///
    /// # Invariant
    ///
    /// The same word as the tag `Op` serialises with: a timeout that named an
    /// operation differently from the frame would send its reader to the wrong line.
    pub fn label(&self) -> &'static str {
        match self {
            Op::Open { .. } => "open",
            Op::Write { .. } => "write",
            Op::Signal { .. } => "signal",
            Op::Pause { .. } => "pause",
            Op::Resume { .. } => "resume",
            Op::Close { .. } => "close",
            Op::Stat { .. } => "stat",
            Op::Read { .. } => "read",
            Op::List { .. } => "list",
            Op::Space { .. } => "space",
        }
    }
}

/// One answer. Exactly one of the two fields is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
}

impl Reply {
    pub fn ok(id: u64, value: Value) -> Self {
        Self {
            id,
            ok: Some(value),
            err: None,
        }
    }

    pub fn err(id: u64, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: None,
            err: Some(message.into()),
        }
    }
}

/// What the runner says without being asked: its greeting, and a shell's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Sent once, before anything else, so a client can refuse a peer it cannot speak to.
    Hello { protocol: u32, pid: u32 },
    Out {
        shell: u64,
        stream: Stream,
        seq: u64,
        data: String,
    },
    /// The shell's pipes ended. `code` is its own status, when the endpoint can know it.
    End { shell: u64, code: Option<i32> },
}

impl Event {
    /// The bytes an `out` event carries; `None` for any other event or a corrupt frame.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        match self {
            Event::Out { data, .. } => crate::base64::decode(data),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Which of a frame's two shapes it is, decided by the field only one of them has.
pub fn frame(value: &Value) -> Frame {
    match value.get("event").and_then(Value::as_str) {
        Some(_) => match serde_json::from_value::<Event>(value.clone()) {
            Ok(event) => Frame::Event(event),
            Err(e) => Frame::Broken(format!("an event this client cannot read: {e}")),
        },
        None => match value.get("id").and_then(Value::as_u64) {
            Some(id) => Frame::Reply(id, value.clone()),
            None => Frame::Broken("a frame with neither an id nor an event".to_string()),
        },
    }
}

/// What one line turned out to be.
#[derive(Debug)]
pub enum Frame {
    Reply(u64, Value),
    Event(Event),
    /// A line that is neither: the connection is no longer trustworthy.
    Broken(String),
}
