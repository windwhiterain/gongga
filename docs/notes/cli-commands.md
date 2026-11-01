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

## Shown

`set_render_session --session <name> [--show true|false]` is the client's answer to
SPEC's UI section, in `cli.toml` beside the sticky arguments: one flag per session
name, `true` for a session with a window beside the list.

The flag is a *reading*, never a deletion. `false` keeps every block the session ever
carried, so `--show true` after it brings the whole history back -- which is the
property the old session filter did not have, because it dropped the lines on the way
in.

A session nobody set is shown. SPEC does not name a default; a window that opens on
nothing, with no line saying why, reads as a broken server, so the default is the one
that shows the work.

`is_shown(name)` is the only question the layout asks, and it is asked of the session
rather than of the map: a session with no entry is shown, so reading the map would
leave a new session with no window.

`stored_show` also reads the words a mode once spelled -- `full` and `line` were on
screen, `none` was not -- so a `cli.toml` written before SPEC had one flag per session
keeps meaning what it meant. A word with no meaning is dropped rather than guessed at,
which shows the session, the same as having no entry.

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
already in the file falls back to `DEFAULT_BLOCK_SIZE` -- a hand-edited file is not a
reason for a client to refuse to start. Its own parse failure is different from the
file's: the first is someone typing, the second is damage.

`DEFAULT_BLOCK_SIZE` is 8, which is SPEC's `initial: 8` for `set_render --block_size`.
It is the answer for a home whose `cli.toml` says nothing, holds rubbish, or is not
there at all, so `BlockSize::default()` and the fallback are one number rather than
two. Before this it was `0`, which meant a fresh home capped nothing while SPEC said
8 -- a default nobody could see and no `cli.toml` could change.

## Sticky::fill

A *required* argument is filled, and so is an optional one the spec declares
`sticky` -- SPEC writes those `(sticky)`, and `set_session --model` is the one that
has it. That flag is what keeps the other optional arguments out: an argument the
command makes optional is a question the caller answered by leaving it out, and
`set_event_ui` is the case that proves the rule: a sticky `session` filling its
`--session` would make every change of block height silently change what is on
screen.

Which arguments those are belongs to the catalog the server publishes, not to the
client: a transparent wrapper knows no command by name.

## Sticky::settle_follow

`set_session` writes the sticky `session` **and** `model` on the acknowledgement, not
on the submission, so a refusal retargets nothing. Both are modes the next command
inherits: the session it acts on, and the model it runs on. A `set_session` that
names no model leaves the remembered one alone, which is what lets `--model` be a
setting rather than a repetition.

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
