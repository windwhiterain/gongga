# cli/commands.rs

## commands.rs

`persistant_cmd_arguments[{name, value}]` is a client-side idea: the CLI keeps
them, fills them into every command before it goes out, and never tells the
server. They persist in `~/.gongga/cli.toml`, so a restarted client still knows
which session you were talking to -- the file, not a running server, is what
remembers.

## Sticky::load

Tolerating a missing or unreadable file is deliberate: a client that cannot
remember its defaults is still usable, and refusing to start over a corrupt
`cli.toml` would make the file a single point of failure for the whole harness.

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
