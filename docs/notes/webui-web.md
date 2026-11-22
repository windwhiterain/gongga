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
finished rows, and so is a block of an `Older`/`Newer` page, so a row is rendered as
exactly one line and the page never re-wraps it. Text bodies (a stream, a tool output) are
the page's to wrap.

What the page must answer, because only drawing knows it:

- `Room` -- rows a pane can draw, at the CSS line height of 20 px, whenever it changes;
- `Columns` -- the width a document row is fitted to. Measured, not assumed: a
  rendered span row's content width divided by the measured advance of the monospace
  face. When a row still wraps to two lines while the pane's box did not change, the
  page narrows that pane by one and states the new width; the accepted width is
  remembered per pane and box so it settles.
- `Held` -- the transcript range a window holds after a trim, as the pair
  `[name, oldest, newest]`. Only the frame that dropped the blocks knows where the
  window now stops.

`Older` and `Newer` are pages, not gestures: `Older` goes above what the pane holds and
`Newer` below. A pane pinned at its oldest row asks for one page (and only one per
visit); a wheel pushed past the oldest or newest row is the gesture that asks again.

## The window is a view, not a copy

The rules are the terminal's, read once for both front-ends: `crates/tui/src/panes.rs`
trim and `crates/tui/src/log.rs` trim/trim_newest/held.
`Render.keep_rows` is a budget -- a pane keeps what it draws plus that margin, and the
frame that drew it drops the rest. Every block carries the transcript rows it came
from (`Row` frames stamp it), so a trim can state the range the window holds with
`Held`; the client's journal then asks the server for what the operator wants again.

The dropped rows are a view change only: the transcript is intact, an older page comes
back through `Older`, a newer one through `Newer`, and the page never guesses a row it
did not receive.

## An opened item is a box

SPEC's `summary` opened is a box: a rule above carrying the item's label, a rule
below, and a body that scrolls inside the `block_size` cap (a body that fits has no
scroll of its own). A folded item shows its summary instead, and a click toggles
between the two -- at the block's own mark, never in the `box_edge` columns.

`box_edge` is a region of the wheel, not a column the box gives up: the box is drawn
at the window's full width, and a wheel in either edge column scrolls the pane rather
than the box. A wheel that lands in the box's own body scrolls the box three rows a
notch (one with the gesture modifier), and once a gesture has chosen its target it
never hands over: `overscroll-behavior: contain` is what makes the browser agree.

## The way back

SPEC draws a "go to bottom" hint only while a view is away from its bottom:
`↓ bottom · Ctrl+↓` in the corner of that pane, and over the session list when the
list is scrolled. Clicking it pins the view to its newest row, the same place
Ctrl+↓ puts it.

## Session list clicks

The shown set is the selection, and the three clicks are the list's whole gesture:

- a plain click shows that session alone (`solo`), and names it the anchor the next
  shift+click spans from;
- Ctrl/Cmd+click adds or removes that one session without touching the others, and
  it also moves the anchor;
- Shift+click selects the visible session rows between the anchor and the clicked
  row, replacing the shown set with that range. Group rows name no session and a row
  inside a collapsed group is not on screen, so the span counts session rows only.
  With no anchor yet, the clicked row starts one. The clicked row is shown last, so
  it is the sticky session.

Each newly shown row first sends `Room` with the pane's measured capacity; the range
sends one `Room` carrying every row it opens, then one solo `render_session` and one
non-solo `render_session` per other row, in that order.

## Deliberate limits

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
