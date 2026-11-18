# crates/webui/web — the page

The window the browser draws. Contract: `crates/client/src/render_wire.rs` for every
frame and intent, `crates/tui` for what each frame means to a window,
`client-render-wire.md` and `client-document.md` for the prose.

## Modules

| file | job |
| --- | --- |
| `main.js` | the socket, the gestures, and the loop between frames and the model |
| `model.js` | the frame state machine: every `RenderMessage`, in arrival order |
| `blocks.js` | blocks and the session tree: keys, folding, marks, the abstract of a folded block |
| `view.js` | DOM: sessions, panes, blocks, quota, command strip, completion popup |
| `cursor.js` | the wire's UTF-8 byte cursors against a textarea's UTF-16 caret, and applying a completion |
| `intents.js` | one `UiIntent` per line, exactly as serde writes it |
| `style.css` | the palette and spacing scale, as tokens |
| `index.html` | the eight script tags and the empty app frame |

## The contract, not a copy of it

Frames are semantic and the client already wrapped a document: a `Document` frame is
finished rows, so a row is rendered as exactly one line and the page never re-wraps it.
Text bodies (a stream, a tool output) are the page's to wrap.

What the page must answer, because only drawing knows it:

- `Room` -- rows a pane can draw, at the CSS line height of 20 px;
- `Columns` -- the width a document row is fitted to. Measured, not assumed: a
  rendered span row's content width divided by the measured advance of the monospace
  face. When a row still wraps to two lines while the pane's box did not change, the
  page narrows that pane by one and states the new width; the accepted width is
  remembered per pane and box so it settles.

`Older` and `Newer` are pages, not gestures: `Older` goes above what the pane holds and
`Newer` below. A pane pinned at its oldest row asks for one page (and only one per
visit); a wheel pushed past the oldest or newest row is the gesture that asks again.

## Deliberate limits

- **No trimming, so no `Held`.** The page holds a window's whole history; the client's
  `keep_rows` budget is a terminal memory concern, and a browser tab has the room.
  `Newer` can still be sent by a push against the newest row, and is answered like any
  other frame.
- **No markdown renderer.** The wire carries rows and span classes, not a document;
  rendering a second markdown engine inside the page would be a second layout with its
  own bugs. A span class says what a run is, and the CSS decides how it looks.
- **No auto-reconnect.** A reload is a new window (see `webui-window.md`); a page that
  silently reconnected would hide whether the client ended.
- **One window per host.** Two windows are two `gongga-webui` processes, each with its
  own `--instance` home, exactly as two terminal windows are two processes.

## Cursors are bytes

`UiIntent::Complete.cursor` and `Completion.start/end` are UTF-8 byte offsets on a
character boundary, while the textarea's `selectionStart` is a UTF-16 code-unit index.
Both directions go through `TextEncoder`/`TextDecoder`; a caret is clamped to a code
point, never split. The completion's edit is the client's own rule
(`crates/client/src/complete.rs`: `Completion::apply` and `expand`), ported rather than
reinvented: item range or token range, snippet stops, caret on the lowest stop, and no
doubled space.
