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

`sticky_cmd_arguments[{name, value}]`, keyed by **type**, never by name: SPEC marks
`Session := path # sticky`, `Model := {provider, model} # sticky`,
`ModelConfig := {...} # sticky` and `SessionConfig := {...} # sticky`, and does not
mark `Workspace`. So the four entries are `session` (a word), `model` (the object, or
the flat `provider/model` word the server also takes), `model_config` and `config`
(objects). `cwd`, `remote`, `thinking` and `keep_alive` are not types here: they are
members, and they travel inside `config`.

### What fills

A slot is filled when it is a **missing argument with no default**: a required
argument outright, and a field SPEC gives no default to. `thinking`, `ctx_limit`,
`cache_tier` (`# re-default if model change`), `cwd`, `remote` and `mode` have
defaults, so no entry reaches them. A struct **update** (`set_session`'s
`config_delta: SessionConfig{..}`) is never filled at all: it says what changes, and
`new_session`'s `config: SessionConfig`, a value, is the one that is filled.

### What a type's value is

A structured type is one value, so a field carrying a type is filled from that
type's entry as a whole, then from the wider entry it sits in, and the line wins:
`model` over `model_config` over `config`. One walk does it (`value_of`), and `prune`
keeps an entry to the fields with no default at every depth -- an entry from an older
build that still holds a `thinking` loses it here rather than sending it.

`fill` fills what the line left open by that rule; an object nobody contributed to is
left out instead of sent empty.

### What a line writes back

An accepted `new_session` or `set_session` writes back what the line stated, and only
that: `absorb` prunes it, merges the whole object into the entry of the argument's own
type (`config`), and gives each field with a kind its own entry (`model_config`,
`model`). Merging is why a delta can say `keep_alive` today and `model_config`
tomorrow without forgetting either. A value the entries supplied is not written back,
because it is already in the file.

`commands::sticky_kinds` is the one list of types that can fill, and `set_sticky`'
refusal, `--type`'s completion and the write-back all read it, so an entry nobody
could set is never recorded either.

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

A structured leaf with an `accepts` set is never reached: `thinking` and `cache_tier`
have defaults by construction, and a defaulted field is not filled. `refused_values`
therefore reads top-level arguments only, which is every slot a fill can reach.

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
