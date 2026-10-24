# cli/commands.rs

## commands.rs

`persistant_cmd_arguments[{name, value}]` is a client-side idea: the CLI keeps
them, fills them into every command before it goes out, and never tells the
server. They persist in `~/.gongga/cli.toml`, so a restarted client still knows
which session you were talking to -- the file, not a running server, is what
remembers.

`event_filters[{name, value}]` lives in the same file for the same reason, and
for one more: what a client chooses to watch is a property of that client, not
of the harness. A per-session "only show main" toggle on the server would have to
be per connection anyway, and then it would be remembered by whoever holds the
connection -- which is the client.

## Sticky::load

Tolerating a missing or unreadable file is deliberate: a client that cannot
remember its defaults is still usable, and refusing to start over a corrupt
`cli.toml` would make the file a single point of failure for the whole harness.

## EventFilters

Each pair says "an event carrying `name` must have value `value`". Two rules make
that usable:

- **Every pair has to match.** Narrowing to `session=main` and then to
  `part=output` should show main's answers, not main's everything plus every
  other session's answers.
- **An event without that field passes.** `--name session` is about the streams
  that belong to a session; a `cmd` echo has no session and belongs to the
  operator who typed it. Dropping it would hide the command that caused the
  output still being shown.

`allows` compares through the serialised event rather than a `match` over every
variant: the field names are the names in the wire JSON (`session`, `part`,
`level`), so a filter value is what a `jq` selector would call it, and adding an
event variant does not need a new filtering branch. A hidden event still returns
`true` for everything else -- see `Client::handle_event`.

## Detail

`set_event_detail --mode` is a display preference, so it is client state: the
server sends the same events either way. It lives in `cli.toml` beside the sticky
arguments and the filters, through the same read-modify-write, so the three
writers still do not clobber each other.

An unknown mode is refused with the list, and an unknown value already in the file
falls back to `all` -- a hand-edited file is not a reason for a client to refuse to
start. Its own parse failure is different from the file's: the first is someone
typing, the second is damage.

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
