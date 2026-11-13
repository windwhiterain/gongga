# client/window_input.rs

The input line's grammar and the window's sticky entries, lifted out of
`crates/tui/src/run.rs` on 2026-11-08. None of it needs a terminal: it reads
`Client`, `State`, the command specs and a `&mut dyn Sink`.

## What moved

| function | what it answers |
| --- | --- |
| `classified` | is this line a command, and what are its tokens |
| `submission` | one line, answered locally or sent |
| `filled` | a command line with its sticky entries |
| `place` | one filled value, where the line spells it or at its end |
| `follow` | what the window keeps for a command the server has not answered |
| `typed_args` | the arguments a line spelled out, before any sticky one |

`submission`'s sink parameter was `&mut TuiSink`; it is now `&mut dyn Sink`, so
a front-end in another language can pass its own `Sink`. `shell-words` moved out
of `crates/tui/Cargo.toml`; the client library already depended on it.

## One substitution

`submission` used to call `TuiSink::say`, an inherent method. `TuiSink::note` is
the same call (`self.0.say(kind, text)`), so a bad parse is now reported through
`Sink::note`, a method every front-end already has and `render_wire.rs` already
encodes. What the operator sees is unchanged.

## Still owed

`sync`, `settle`, `declare` and the `answer_*` functions stay in
`crates/tui/src/run.rs`: they read the window's journal (`ui.stamp_of`,
`ui.drawn`, `ui.room_of`, `ui.take_settled`). They move with the journal, not
before it. The `interactive` loop itself is still there. See `DESIGN.md` §1.

## A structured argument is filled leaf by leaf

`--config` is SPEC's `SessionConfig`, one JSON object. A sticky entry may name any
leaf of it -- `model`, `thinking`, `ctx_limit`, `cache_tier`, `remote`, `cwd`,
`keep_alive` -- and the line keeps every leaf it stated. `place` rewrites the
object where the line spells it instead of appending a second `--config`, which
the parser refuses as given twice.

`follow` is the other direction: an accepted `new_session` or `set_session` writes
back every leaf the line stated, under the entry its type answers to. A value the
entries filled in is not written back, because it is already in the file.

Only the leaves SPEC gives no default to are filled at all (`model`, `keep_alive`);
the rest are the type's decision, not a missing argument (`docs/notes/tui-state.md`).
