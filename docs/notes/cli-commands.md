# cli/commands.rs

## commands.rs

`persistant_cmd_arguments[{name, value}]` is a client-side idea: the CLI keeps
them, fills them into a command that requires the argument and does not carry
it, and never tells the server. They persist in `~/.gongga/cli.toml`, so a restarted client still knows
which session you were talking to -- the file, not a running server, is what
remembers.

`event_filters[{name, value}]` lives in the same file for the same reason, and
for one more: what a client chooses to watch is a property of that client, not
of the harness. A per-session "only show main" toggle on the server would have to
be per connection anyway, and then it would be remembered by whoever holds the
connection -- which is the client.

## File::connection, Remembered

`connect_server` has to outlive the process that ran it -- a one-shot command has
nothing else to remember it with -- so the last address lives in `cli.toml`
beside the sticky arguments. The IP is parsed back as an address rather than
formatted into one: `"::1:9700"` is not a socket address, and a fallback to the
default would have silently sent later commands to the wrong port.

## Sticky::load

Tolerating a missing or unreadable file is deliberate: a client that cannot
remember its defaults is still usable, and refusing to start over a corrupt
`cli.toml` would make the file a single point of failure for the whole harness.

## RenderModes

`set_render_session --session <name> [--mode none|line|full]` is the client's answer
to SPEC's UI section, in `cli.toml` beside the sticky arguments: one mode per
session name.

A mode is a *reading*, never a deletion. `none` keeps every block the session ever
carried, so `line` or `full` after it brings the whole history back -- which is the
property the old session filter did not have, because it dropped the lines on the
way in.

A session nobody set is `full`. SPEC lists the three modes and does not name a
default; a window that opens on nothing, with no line saying why, reads as a broken
server, so the default is the one that shows the work.

`named(mode)` is only for the file's own entries. The announcement does not use it:
a session with no entry is `full`, so `Client::layout` reads the mode off each
session instead of reading the map.

## BlockSize

`set_event_ui --block_size` is a display preference, so it is client state: the
server sends the same events either way. It lives in `cli.toml` beside the sticky
arguments and the filters, through the same read-modify-write, so the writers still
do not clobber each other.

It is one number, not a mode list. The setting it replaces (`set_event_detail
--mode all|line`) had to name a *kind* of behaviour per mode, and the two names
only ever differed by whether a block folded; a count of lines says the same thing
with no vocabulary to memorise, and `0` is the only word for "no cap".

SPEC words the cap as one on a *non-output* block: an answer is never capped, so
the number carries its own exception and the setting stays one number rather than
two switches that can disagree.

Without an argument the command is a read: it prints the size, the session filter and
the split, and changes nothing. Clearing the cap is `--block_size 0`, said on purpose
-- a command that both reads and resets depending on how much you type is the kind of
thing an operator learns the hard way.

An unparseable value is refused with the way to say one, and an unknown value
already in the file falls back to `0` -- a hand-edited file is not a reason for a
client to refuse to start. Its own parse failure is different from the file's: the
first is someone typing, the second is damage.

## Sticky::fill

Only a *required* argument is filled. An argument the command makes optional is a
question the caller answered by leaving it out, and `set_event_ui` -- which now takes
`--session` beside `--block_size` and `--split` -- is the case that proves the rule: a
sticky `session` filling it would make every change of block height silently change
what is on screen. `send_message --session` is required, so the sticky value still
routes every message the way SPEC's auto-set line says.

## EVENT_FIELDS

An `--name` that no event carries filters nothing, and silently doing nothing is
how a user concludes the log is broken. The list is the wire vocabulary, and
`set_event_filter` refuses anything outside it with the list in the message.

`session` and `sessions` both appear because both are on the wire: the first
tags a stream, the second is the session list event. They mean different things
on purpose; `session=main` does not hide the list.

## parse_args

The value is whatever follows its flag, *including* a token that starts with
`--`. Refusing those made the one thing a message flag exists for impossible:
`--message "--version"` was read as a missing value, and the only way through
was `--message=--version`, which nothing advertises.

The cost is the other direction: a forgotten value swallows the next flag, which
then fails on whatever follows it -- `--session --message hi` reads `--message`
as the value of `--session`, and `hi` is what fails.

## a_value_may_start_with_dashes

`send_message` exists to carry arbitrary text, and `--version` is text: the
parser therefore cannot use "starts with a dash" to decide whether a token is a
value.
