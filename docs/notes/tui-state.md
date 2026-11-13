# client/window_state.rs — the window's own state

The source moved from `crates/tui/src/state.rs` into the client library. The note
keeps its name because it is about the window's state, not about a crate.

The window's own state, in `<client-home>/client.toml` -- by default
`~/.gongga-client/client.toml`. Everything the operator sets once and expects to find
again lives here: the sticky entries, which sessions are drawn, the block size and the
box edge, and the address `connect_server` last pointed at.

It lives in the client home, not the harness home, because it is the window's and each
window has its own home: two windows shared one file only because they shared a
directory, and the last writer won. The home is already claimed by `client.lock`
([`home-lock.md`](home-lock.md)), so one home has one window and one config.

`config.toml` and `server.key` stay in the harness home: those are the server's, and
every window dialling that server reads the same ones.

A home that predates the move is not reset: `window_state::adopt_legacy` copies
`<harness-home>/client.toml` into a client home that has no file of its own, once, and
leaves the original for a build that still reads it. A client home that already has a
file is never rewritten from the old one.

The rule this file exists for: **client state is the window's**. A one-shot
`gongga-cli` reads none of it and writes none of it -- it is a script or a model that
names every argument it means -- so `client.toml` has exactly one reader and one writer.
Before this it had two of each: an agent's `set_session` wrote the operator's sticky
session, and an agent's bare `send_message` landed in whatever session that entry
named.

## One file, one writer per key

Each structure reads the file, replaces its own key, and writes it back, so a
`set_render` never clobbers the sticky entries it just read past. That is also why
the file is not rewritten from a single in-memory copy: a stray second writer -- a
build from another worktree, a hand edit -- would be silently dropped.

A missing or unreadable file is never fatal. A client that cannot remember its
defaults is still usable, and refusing to start over a corrupt `client.toml` would make
the file a single point of failure for the whole harness. An unparseable *value* in
an otherwise readable file falls back to the default the type publishes.

## Sticky

`sticky_cmd_arguments[{name, value}]`, keyed by argument **kind**, never by name:
`set_session --name <session>` and `get_messages --session <session>` share one
`session` entry whatever their flags spell, and an argument with no kind never fills
(which is what keeps `--must_create` out). The kinds that exist are the ones the
published catalog declares.

`fill` answers exactly one question: **is this a value the caller was obliged to
supply and did not?** So a required argument is filled from its entry, and an argument
with a default of its own is not -- whichever way that default is spelled. An argument
no entry names keeps that default, applied where the command runs.

A structured argument -- `--config`, SPEC's `SessionConfig` -- is filled leaf by leaf
under the same rule, and only when it is a *value*: `new_session`'s config is one, and
`set_session`'s is SPEC's `SessionConfig{..}`, a struct update that says what changes
and is never filled. What an update states is written back instead. and the published schema states which leaves have defaults
(`StructField.default`, SPEC's `provider_default()`, `70%`, `none`, `.`, `false`).
Only the rest fill: `model` and `keep_alive` in today's catalog. `thinking`,
`ctx_limit` and `cache_tier` are marked `# re-default if model change`, and `cwd`
and `remote` have defaults of their own -- an entry typed days ago may not stand in
for any of them. What the line stated stays, and the object is built only when an
entry contributed to it.

`leaf_value` reads an entry as the leaf's own shape: a switch is a boolean, a model
is the object SPEC spells (or the flat `provider/model` word the server also takes),
everything else stays the text it was stored as.

So one list of kinds has two sources -- the `kind` of a required argument, and the
name of a leaf without a default -- and `client::commands::sticky_kinds` is the only
place that reads both: `set_sticky` refuses a type outside it, `--type` completes it,
and only a type inside it is written back when a line states it.

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

The same question is asked of a structured leaf, because sticky `thinking = low`
reaches a session through `--config` just the same: the leaf's own `accepts` is
checked against the model the filled call runs on -- the sticky model if that is the
one it names. A refused leaf is cut out of the object, the rest is sent, and one
line says which entry was left to the model's own.

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
