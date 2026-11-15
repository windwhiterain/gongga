# client/window_refill.rs

The client owns the journal and reads it through `WindowFacts`; the TUI reports
pane heights and gestures through `UiIntent`. No refill queries a terminal.

`declare` validates cache generations and bounds against the greeting. Invalid
journals are deleted without removing the pane's visibility or height.

`answer_transcripts` visits visible panes only. A fresh view restores the cached
portion of its current tail once. An older disconnected cache is retained on disk,
not walked forward through every intervening page. Missing rows are requested by
range, with one request in flight per session.

`answer_older` uses the rendered floor, independently of the disk cache's floor.
It reads a cached contiguous page first, otherwise requests the page immediately
above the view. Prepending preserves bottom-relative scroll, so receiving history
does not manufacture another scroll gesture.

Rows and unfinished snapshot parts carry their request id and wait for the
command result. Live rendering events wait separately while the global stream
cursor advances on receipt. `as_of` joins the snapshot to the live stream, even
when a covered event arrives after the result. Transcript metadata is applied in
stream order rather than replayed from that rendering buffer.

`answer_completion` sends only missing directory listings and notifies the UI
when their version changes.

## Two pages, one fill (2026-11-15)

A window is bounded at both ends, so it is owed pages in both directions: `UiIntent::Older`
asks for the rows above its floor and `UiIntent::Newer` for the rows after its newest one.
`answer_newer` mirrors `answer_older`, and `Client::ask_newer` anchors the page at
`drawn + 1`, capped at the server's own end -- nothing is asked for a window that never
received a page (`loaded`), because what an empty window is owed is the *tail*. `Fill`'s
two booleans became one `FillKind` (`Whole`, `Tail`, `Older`, `Newer`): the pages never
rewind and never restamp the end they are not anchored on.
