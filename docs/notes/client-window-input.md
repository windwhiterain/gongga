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

## A structured value is filled by its type

`--config` is SPEC's `SessionConfig`, one JSON object, and a sticky entry holds that
type as a whole (`config`), or the narrower types a field carries (`model_config`,
`model`). `place` rewrites the object where the line spells it instead of appending a
second `--config`, which the parser refuses as given twice.

`follow` is the other direction: an accepted `new_session` or `set_session` writes
back what the line stated -- `Sticky::absorb`, pruning the fields that have defaults
and merging each stated value into the entry of its own type. A value the entries
supplied is not written back, because it is already in the file. A struct *update*
(`set_session`) is never filled; a value (`new_session`) is.

The same accepted command marks a session it *created* as this client's own: the
name is one the server's table did not hold when the line was sent, so joining an
existing session -- another client's, a subagent's -- writes no mark. Both halves
wait for the answer, as one `Follow { sticky, created }`.
