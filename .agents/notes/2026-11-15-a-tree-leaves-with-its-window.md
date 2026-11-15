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

Answered in "What the next session did", below.
- The TUI still builds trees for windows it is not drawing until they leave the frame;
  dropping those writes outright is the remaining half of "a front-end holds only what
  it draws".
- `cargo build` could not relink `gongga-tui.exe` while the operator's window was
  running; this change was built and verified in `.dev/target-tc`.

## What the budget does not bound, and why it needs the downward page

The page below exists now; this section is why it was needed.

`Panes::trim` runs with `keep_rows + pane height + pane scroll`, so the margin follows
the scroll: paging further up always grows the window again. What is bounded today is
the case that grows without a gesture -- a session streaming for hours. A strictly
constant window needs the other half:

1. An intent that asks for the page *after* the rows a window holds (`UiIntent::Newer`,
the mirror of `Older`), with `Room`/`Columns` already carrying the size.
2. The client answering it from the journal first and from the server second, exactly
   as `answer_older` does, and stamping the result with `extend_drawn`'s mirror -- the
   held range grows at the newest end instead of the oldest.
3. The front-end appending that page and, only then, trimming the oldest blocks (the
   budget no longer needs `scroll` in it, because nothing above the pane is kept).

## Why `prepend` is O(all blocks)

`Log::prepend` splices into `entries: Vec<Entry>` and calls `reindex()`, which renumbers
every block of every window: `Entry::at` and each ledger's `blocks` are positions in
that one vector. The cost is the flat, globally-indexed storage, not the splice. Making
a page O(log n) means giving each window its own list of blocks (handles into an arena)
and letting a position live in a per-window structure -- a change to how the log stores
things, worth doing together with putting `seq` on a block, since that is the same
"a block has a place that is a fact" idea.

## Left undone in the session that wrote this

- Nothing measured for the budget: to cross 800 rows in one live turn the harness needs
  an answer payload larger than a single command-line word (Windows truncates at ~8 KB),
  so it needs either a `--answer-lines` file option in `scripts/tui_live.py` or a fake
  endpoint that streams N rows. The budget's *rule* is also the thing to change first
  (above), which makes the measurement meaningful.
- The downward page and the per-window block storage are not started. Everything that is
  committed compiles, and the two frames checks (a real 160-call transcript walked to the
top, and hide-then-show) pass on the build that carries them.

## What the next session did (same day)

- **The downward page exists.** `UiIntent::Newer` is `Older`'s mirror; the client answers
  it with `ask_newer`/`FillKind::Newer` and the page lands below the rows
  (`RenderMessage::Newer`, `Log::append_page`).
- **The budget no longer follows the scroll.** `Panes::trim` keeps one margin on each
  side of the viewport: `Log::trim_newest` drops the newest blocks, whole blocks only, and
  never one the pane is drawing.
- **A window states its range.** `RenderMessage::Row` and `PastRow`'s `seq`/`from` give
  the front-end the transcript's own coordinates, and `UiIntent::Held` tells the client
  where the window now stops; `Sync::short` is what keeps that from looking like being
  behind.
- **Measured, with `keep_rows = 20`** on the live harness and eight short turns (the
  default 800 needs an answer larger than a command-line word, which is why the harness
  grew no option for it): every frame moves to the turns either side with no repeated and
  no skipped turn, and a push past the newest row costs one `get_messages`. Numbers in
  `.dev/verify/<sha>-window-budget.json`.
- **Still open**: the O(n) `prepend`/`splice` + `reindex` noted below, and the operator's
  own window must be restarted to run any of this.
