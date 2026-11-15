# A tree leaves with its window

Reported: the TUI often draws no `tool call` row, only its result, and an answer
sometimes appears under the previous answer instead of at the newest row.

## What was measured first

- A real 160-call transcript, rendered by a fresh window and walked to the top: every
  `✓/✕` row had its `▸` row above it. Twice, also after a hide/show. So the drawing
  side does not drop calls when a window is built from a page of rows.
- The window's own client journals pair every `tool_answer` with its `tool_call`, and
  the home's 12 639 `tool_call` rows have no repeated call id within a session.
- So the defect is not "a call row is forgotten"; it is the state a window is in when
  it was built live and then switched away from and back.

## Two defects behind it

1. **A frame overtook the queue.** The answer's `Document` was built and sent at once
   by `PipeFrontend::emit_document`, while every other drawing frame waited in the
   sink's `WireSink` until the sink was dropped. In a refill, `Clear` travels through
   that queue, so the document arrived *first* and was wiped: the window then showed a
   turn with no output at all. Fixed by having `answer`/`rewritten` return the frame
   and the sink queue it after the row's own `Stream`.
2. **A hidden window kept a tree the client did not know about.** A front-end drew
   every window's rows, hidden or not, while the client counted a window as loaded
   once it had sent a page. Showing it again therefore drew either a stale tree or
   (after this change dropped it) nothing. Fixed by making "drawn is held" the rule on
   both sides: `Panes::layout` drops the log of a window that leaves the frame, and the
   client's `set_shown` declares a window that joins or leaves empty (in memory and in
   the journal stamp).

Measured after: hide then show a session whose turn had run gives the transcript's
rows in order -- `blocks = 6`, `total = 17` for rows 1, 4, 5, 6, 7, 8, 9 -- where it
previously gave five rows and no answer.

## The other half: a turn's second output (same day)

The stream key was `r:<session>:<turn>:<part>`, with no step, so the text a model writes
before a tool call and the text it writes after it were one block: the newest output was
drawn inside the older block, which is the other half of "not at the newest row".
`ResponseEvent` now carries `step` -- the request of the turn the part arrived in, which
the transcript already stores (`messages.step`) -- and the key is
`r:<session>:<turn>:<step>:<part>`. Both the live publish and `row_event` fill it, so a
turn with two steps is two blocks and a rebuilt window agrees with the live one.

Measured: the same turn now draws `answer(step 0)`, `reasoning(step 1)`, `answer(step 1)`
in that order, where it drew one merged answer before; and hiding then showing the
session repaints nothing at all -- the rebuilt window is byte-identical to the live one.

A row this build cannot place keeps `step: 0`: an item still open when a window
reconnected, an interrupted item's stored row, and the operator's own lines.

## Open
- The TUI still builds trees for windows it is not drawing until they leave the frame;
  dropping those writes outright is the remaining half of "a front-end holds only what
  it draws".
- `cargo build` could not relink `gongga-tui.exe` while the operator's window was
  running; this change was built and verified in `.dev/target-tc`.
