# client/window_state.rs — the window's own state

The source moved from `crates/tui/src/state.rs` into the client library. The note
keeps its name because it is about the window's state, not about a crate.

The window's own state, in `~/.gongga/tui.toml`. Everything the operator sets once
and expects to find again lives here: the sticky entries, which sessions are drawn,
the block size and the box edge, and the address `connect_server` last pointed at.

The rule this file exists for: **client state is the window's**. A one-shot
`gongga-cli` reads none of it and writes none of it -- it is a script or a model that
names every argument it means -- so `tui.toml` has exactly one reader and one writer.
Before this it had two of each: an agent's `set_session` wrote the operator's sticky
session, and an agent's bare `send_message` landed in whatever session that entry
named.

## One file, one writer per key

Each structure reads the file, replaces its own key, and writes it back, so a
`set_render` never clobbers the sticky entries it just read past. That is also why
the file is not rewritten from a single in-memory copy: a stray second writer -- a
build from another worktree, a hand edit -- would be silently dropped.

A missing or unreadable file is never fatal. A client that cannot remember its
defaults is still usable, and refusing to start over a corrupt `tui.toml` would make
the file a single point of failure for the whole harness. An unparseable *value* in
an otherwise readable file falls back to the default the type publishes.

## Sticky

`sticky_cmd_arguments[{name, value}]`, keyed by argument **kind**, never by name:
`set_session --name <session>` and `get_messages --session <session>` share one
`session` entry whatever their flags spell, and an argument with no kind never fills
(which is what keeps `--must_create` out). The kinds that exist are the ones the
published catalog declares.

`fill` is SPEC's two bullets as one rule: an omitted non-optional argument is filled
from its entry, and an omitted optional argument takes the entry as its initial
value. An argument no entry names keeps the command's own initial, applied where the
command runs.

## Sticky to xxx

SPEC's `sticky to `<kind>`` -- `--thinking` and `--ctx_limit` follow `model`,
`--cwd` follows `remote` -- is data on the published argument (`ArgSpec.sticky_to`),
not a table here. Setting an entry forgets the entries that follow it, and so does
clearing it; a command that carries both is the one case where the follower is
written after its parent and survives.

Two details that were bugs until they were written down:

- **Only a different value is a change.** Re-typing the model a session already had
  is not a reason to forget the level that belongs to it.
- **Only what the operator typed is written back.** `set_session --model B` with a
  sticky `thinking = low` sends `thinking` too (the fill supplies it), but writing
  `low` back as if it had been stated would keep it alive across the model change --
  which is exactly the mismatch the rule is for.

## The stale entry and the closed set

A remembered level is not a level the next model takes. The published argument says
what its command accepts (`ArgSpec.accepts`, see `docs/notes/protocol-message.md`),
so the window drops a filled value that set does not take and says so in one line.
A value the operator *typed* is sent as typed and judged by the server: that one is
a statement, and a statement is worth a refusal.

## Shown

`render_session[{name, value}]`: `true` for a session drawn beside the list. It is a
view, never a deletion -- a session left hidden keeps every block it ever carried --
and a session nobody set is hidden, because the window draws what it was told to
draw (2026-11-05, the operator's ruling). `stored_show` still reads the words a mode
once spelled (`full`, `line`, `none`), so an older file keeps meaning what it meant.

Showing a session makes it the sticky one; hiding the sticky one clears the entry.
That is the same rule the click uses, so a session the operator named and a session
a command named end up on screen the same way.

## BlockSize and BoxEdge

Two numbers, each with one default constant, because the file may say anything:
`DEFAULT_BLOCK_SIZE` (8, SPEC's `initial: 8`) and `protocol::DEFAULT_BOX_EDGE` (2).
`0` is a value the operator can type and the only way to say "no cap"; a fresh home
never starts at it, which is what the constants are for.

## connection

`connect_server` writes the address before the window is told, so a redial at once
reads the new one. It is read exactly once, when the window starts: from then on the
address in force is the one this process dials, not the one a file says. The IP is
parsed back as an address rather than formatted into one -- `"::1:9700"` is not a
socket address, and a fallback to the default would have sent later dials to the
wrong port.
