//! The serialized rendering vocabulary a client daemon speaks to its front-ends.
//!
//! See `docs/notes/client-render-wire.md`.

use serde::{Deserialize, Serialize};

use crate::{Candidate, Layout, LineKind, Sink, protocol};

/// One run of a document row: its words, and the class that says how they look.
///
/// # Invariant
///
/// A class is what a run *is* (strong, code, a marker), never a colour or a font: the
/// front-end owns the ink, and two front-ends drawing one answer differ only there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSpan {
    pub text: String,
    pub class: SpanClass,
}

impl DocumentSpan {
    pub fn of(text: impl Into<String>, class: SpanClass) -> Self {
        Self {
            text: text.into(),
            class,
        }
    }

    /// The same words, drawn as a heading.
    pub fn heading(self) -> Self {
        Self {
            class: SpanClass::Heading,
            ..self
        }
    }

    /// The same words, drawn as a heading or an emphasised run.
    pub fn strong(self) -> Self {
        match self.class {
            // A heading's words are often already strong: one class says it once.
            SpanClass::Text => Self {
                class: SpanClass::Strong,
                ..self
            },
            _ => self,
        }
    }
}

/// What a run is, as SPEC's `colored markdown` spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanClass {
    Text,
    /// A heading is not a bold word: a front-end draws it as one, and the words it
    /// holds read as strong inside it.
    Heading,
    Strong,
    Emphasis,
    Strike,
    Code,
    /// A line of a fenced block: the same words as `Code`, drawn as a block rather
    /// than as a word in a sentence.
    CodeBlock,
    /// A list's bullet, a quote's bar, a heading's own mark.
    Marker,
    /// An address, printed after the words that carry it.
    Link,
}

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
    /// The window's own cap and margin: why a block stops growing, and how much of
    /// its width a box gives back.
    ///
    /// # Invariant
    ///
    /// Sent when it changes, and on the first sync after a front-end attaches: the
    /// setting lives in the client, and the process that draws is another one.
    Render {
        block_size: usize,
        box_edge: usize,
    },
    /// A body as finished rows: SPEC's `output`, already parsed and already fitted to
    /// the width the front-end stated.
    ///
    /// # Invariant
    ///
    /// The rows are the whole body, not the part that grew: a document is derived from
    /// its source, and one more delta can change a row above the last one, so a
    /// front-end replaces what it holds by `key` instead of appending. See
    /// `docs/notes/client-document.md`.
    Document {
        window: String,
        key: String,
        kind: LineKind,
        tag: String,
        rows: Vec<Vec<DocumentSpan>>,
        done: bool,
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
    Rewind {
        window: String,
    },
    StickySession {
        name: Option<String>,
    },
    Stopped {
        session: String,
        failed: bool,
    },
    /// The unsent line goes back into the input, not into a note.
    Draft {
        text: String,
    },
    /// The header's own / not own label, already spelled for the operator.
    Server {
        label: Option<String>,
        ours: bool,
    },
    /// `connect_server` moved the address: drop the old endpoint's rows.
    Repoint {
        endpoint: String,
    },
    /// The command vocabulary changed: ask again for the current line's words.
    Vocabulary {
        version: u64,
    },
    /// The candidates for one `Complete` intent, and the token they replace.
    ///
    /// # Invariant
    ///
    /// The answer carries the line and caret it was asked about, because a window
    /// keeps reading keys while the round trip is out: an answer for the words that
    /// were there is not an answer for the words that are.
    Completion {
        line: String,
        cursor: usize,
        start: usize,
        end: usize,
        subject: Option<String>,
        candidates: Vec<Candidate>,
    },
    /// A page of the past, to be drawn *above* what the window already holds.
    ///
    /// # Invariant
    ///
    /// One frame, finished rows: a page is older than every row the window has, so
    /// a front-end that appends it draws the past as the newest thing on screen.
    /// It carries the page as an order instead of a stream of calls, because a
    /// page has already happened: nothing of it will be rewritten. See
    /// `docs/notes/client-render-wire.md`.
    Older {
        window: String,
        rows: Vec<PastRow>,
    },
}

/// One finished row of a page of the past: a line, or a block that already ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "row", rename_all = "snake_case")]
pub enum PastRow {
    Line {
        kind: LineKind,
        text: String,
    },
    /// A block that streams under `tag`, kept by `key` while it lived.
    Block {
        key: String,
        kind: LineKind,
        tag: String,
        text: String,
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
    /// How many rows each pane could draw: what one page of the past is sized by.
    ///
    /// # Invariant
    ///
    /// Sent when a pane's height changes, never per frame: only the drawing knows
    /// how tall a window is, and the page a wheel above the first row asks for is
    /// that height, not a constant. See `docs/notes/client-window-journal.md`.
    Room { windows: Vec<(String, usize)> },
    /// How many columns each pane can draw: the width a document is fitted to.
    ///
    /// # Invariant
    ///
    /// A document is wrapped once, here, so every front-end of one answer sees the
    /// same rows. Sent when a pane's width changes, for the same reason `Room` is.
    Columns { windows: Vec<(String, usize)> },
    /// The client answers with candidates, so syntax stays out of the UI.
    Complete { line: String, cursor: usize },
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
    /// A page of the past, drawn above what the window holds.
    ///
    /// # Invariant
    ///
    /// A frame, not a cursor message: where the page goes is the front-end's to
    /// decide, and the daemon's own record of it is `landed`. See
    /// `docs/notes/client-render-wire.md`.
    pub fn prepend(&mut self, window: &str, rows: &[PastRow]) {
        self.render.push(RenderMessage::Older {
            window: window.to_string(),
            rows: rows.to_vec(),
        });
    }

    /// Queues a frame another part of this process built, in the order the rest
    /// of the same row's frames were queued.
    ///
    /// # Invariant
    ///
    /// There is one queue per sink call: a frame sent around it can arrive before
    /// the `Clear` that was meant to precede it, which is a row drawn into a
    /// window that then empties. See `docs/notes/client-pipe.md`.
    pub fn push(&mut self, message: RenderMessage) {
        self.render.push(message);
    }

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
        self.render.push(RenderMessage::Rewind {
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

/// One JSON value per line: a frame ends at the newline, never inside a string.
///
/// # Invariant
///
/// `serde_json` escapes a newline inside a string, so a submitted line holding
/// paragraphs is still one frame. See `docs/notes/client-render-wire.md`.
pub fn encode_frame(message: &RenderMessage) -> anyhow::Result<String> {
    Ok(serde_json::to_string(message)? + "\n")
}

/// Reads one intent from a line the front-end wrote.
pub fn decode_intent(line: &str) -> anyhow::Result<UiIntent> {
    Ok(serde_json::from_str(line)?)
}

/// Writes one intent as one JSON line, the mirror of `encode_frame`.
pub fn encode_intent(intent: &UiIntent) -> anyhow::Result<String> {
    Ok(serde_json::to_string(intent)? + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_frames_round_trip() {
        let frames = vec![
            RenderMessage::Draft {
                text: "keep this line\nand this one".to_string(),
            },
            RenderMessage::Server {
                label: Some("own #42".to_string()),
                ours: true,
            },
            RenderMessage::Server {
                label: None,
                ours: false,
            },
            RenderMessage::Repoint {
                endpoint: "127.0.0.1:9000".to_string(),
            },
            RenderMessage::Vocabulary { version: 7 },
            RenderMessage::Completion {
                line: "/set_session{\"id\":\"m".to_string(),
                cursor: 18,
                start: 14,
                end: 17,
                subject: Some("id".to_string()),
                candidates: vec![Candidate::word("main", "a session")],
            },
        ];
        for frame in frames {
            let encoded = encode_frame(&frame).unwrap();
            assert_eq!(encoded.lines().count(), 1);
            assert_eq!(
                serde_json::from_str::<RenderMessage>(&encoded).unwrap(),
                frame
            );
        }
    }

    #[test]
    fn a_complete_intent_carries_the_line_and_cursor() {
        let intent = decode_intent(r#"{"intent":"complete","line":"/set m","cursor":6}"#).unwrap();
        assert_eq!(
            intent,
            UiIntent::Complete {
                line: "/set m".to_string(),
                cursor: 6,
            }
        );
    }

    #[test]
    fn a_room_intent_names_a_pane_and_its_rows() {
        let intent = UiIntent::Room {
            windows: vec![("one".to_string(), 24)],
        };
        let encoded = encode_intent(&intent).unwrap();
        assert_eq!(decode_intent(&encoded).unwrap(), intent);
    }

    #[test]
    fn an_intent_round_trips_as_one_line() {
        let intent = UiIntent::Submit {
            line: "one\ntwo".to_string(),
        };
        let encoded = encode_intent(&intent).unwrap();
        assert_eq!(encoded.lines().count(), 1);
        assert_eq!(decode_intent(&encoded).unwrap(), intent);
    }
}
