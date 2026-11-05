# lazy: pull what the frame draws, and nothing else

User's verdict (2026-11-05, twice): the stream must be **delta only + lazy retrieve**, and
the retrieve must be "需要什么拉什么，不渲染的不拉". What ships today does neither in
full:

- a window with no journal declares `drawn = 0`, so its first frame asks for
  `get_messages --replay --range 1:` — **the whole transcript at once** (measured: 3000
  rows ≈ 3000 events ≈ 10 s);
- a stream jump sets `Drawn::Lost`, whose answer to "what do I owe" is `Some(1)` — the
  whole transcript **again**. While a big fill is applied the connection stops reading its
  `bus_rx`, the 4096-event broadcast buffer overflows, the next `seq` jumps, `Lost` is set,
  and the loop starts over. That is why a session "时不时请求一大堆 events" with nobody
  scrolling.

## The rule

A window holds exactly the rows a frame has drawn, plus the ones it has already been
given. It asks for a row range only when a frame is about to draw that range. "Page" is
never a constant — SPEC forbids a budget — it is what the pane needs at that moment.

## Shape (what to build)

1. **A page is a range, not a transcript.** `Client::ask_transcript(name, from, to)` and the
   fill remembers the range. A fill's reply already carries `as_of` and `last_seq`; it needs
   nothing else.
2. **`Sync` learns two rows, not one.** Today's `drawn` is the *bottom* watermark ("every
   row ≤ this is drawn"). Add `floor`: the oldest row the window holds (`0` = none). The
   first frame of a fresh window asks for `-N:` where `N` is the rows the pane can show; a
   frame scrolled above `floor` asks for `(floor - N):(floor - 1)` and **prepends**.
3. **`Drawn::Lost` goes away.** A jump does not invalidate what was drawn: keep `drawn` and
   set `stale = true`, whose answer is `drawn + 1` — a tail read, which is also the only
   thing that can carry the part still streaming. `settle_fill` clears `stale`. This alone
   kills the runaway.
4. **Prepend.** `Log::prepend(window, blocks)` inserts at the window's first block and calls
   `reindex()` (indices are global, so every ledger shifts). The journal is the client's own
   record, not the transcript's: for a prepend it is **rewritten** for that window in the
   new order (`Cache::rewrite(window, &[Record])`), which keeps `replay` a forward walk and
   needs no ordering metadata. The window's `floor` is journaled as `Record::Floor { seq }`,
   so a restart knows where its oldest row is.
5. **Scrolling is the only trigger.** `answer_transcripts` asks for the range the frame is
   about to draw in that window — never for a range nothing draws. A hidden session costs
   nothing; a session scrolled to the bottom costs nothing; the row above the top costs one
   page when PageUp reaches it.
6. **Streaming keeps its own path.** Rows that arrive live still append at the bottom; the
   `drawn` watermark advances on `Event::Transcript` exactly as it does now.

## Where

`crates/cli/src/lib.rs` — `Sync`, `Drawn`, `Fill`, `behind_one`, `ask_transcript`,
`settle_fill`, `handle_delta`.
`crates/tui/src/{log,cache,panes,run}.rs` — `Log::prepend`, `Cache::rewrite`/`Floor`,
`Panes::floor_of`, the draw-time request, the "rows above are not loaded" top row.

**Warning:** another agent is moving `crates/cli` to `crates/link` (uncommitted at the time
of writing). Rebase on that move, or land before it — do not edit both trees.

## Verify (a script, not a claim)

- `huge.sh` (already in `.dev/verify`): a 3000-row transcript, a fresh window. It must draw
  **one pane's worth** and go quiet, and the server log must show one `get_messages` with a
  tail range, not `1:`.
- PageUp at the top: exactly one more `get_messages`, for the range above `floor`.
- A streamed turn with a big fill in flight: no second `get_messages` after the first
  (the `stale` fix), and the window still ends at the transcript's own end.
- `grep -c "client fell behind" <server log>` is the observable for the runaway; it must
  stop growing once `stale` replaces `Lost`.

## Landed (2026-11-05)

Points 1-6 above are in, with four corrections the code forced:

- **`Log::prepend` is not a call, it is a position.** The link tells the front-end that
  the rows arriving are older (`Sink::older`), `Log::older` parks a cursor at the window's
  front, and `attach` inserts each block there and re-sums that window. The journal is
  rewritten when the page is closed (`Sink::landed`), not when it starts.
- **`Floor` marks a page's *start*, not the window's floor.** One record per page,
  written in front of the block that page began with, so a restart replays one page and
  knows exactly which rows it holds. `Log::floor_of` answers from memory
  (`ledger.pages.first()`), which is the only honest answer: the journal can reach further
  back than a seed read.
- **The scroll is an event, not a poll.** PageUp changed a pane and produced no `UiEvent`,
  so the loop that owns the connection never woke (`scripts/tui_live.py` run: 26 presses,
  zero requests). `UiEvent::Older(Vec<String>)` is taken, not read, by `next_event`; the
  loop holds the pages owed and asks one page per press.
- **A journal with no page boundary is refilled whole** (`Sink::forget` on the first fill),
  because a page appended to rows the window cannot name would land on rows it has.

User's verdict (2026-11-05): of A (replay the whole journal at startup), A′ (journal the
page boundary) and B (treat a restart as a new window), he asked why A needed a replay at
all -- it does not -- and approved **A′**: one page at startup, exact floor, no guessing.

Measured (`.dev/verify/older.sh`, `035724d-older.json`): 400-row transcript, 22-row pane,
26 presses -> 18 page fills for 400 rows, each frame one page, no row twice, restart reads
back one page and then asks the server for the rows above it (19 -> 37 `get_messages`).
Mutation: declaring no floor turns the restart phase red (19 -> 19).

Not landed, and not mine to decide alone: merging `fix/older` into `main` while another
agent has the same checkout on `main` with uncommitted server work in flight.
