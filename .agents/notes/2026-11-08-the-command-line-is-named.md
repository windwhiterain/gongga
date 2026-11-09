# The command line is named

Verdict (2026-11-08): every argument of a `gongga-cli` line is spelled `--name`,
including the ones the catalog marks positional. `python/gongga_cli.py`'s `bind()`
refuses a bare word and answers with the flags the command does take.

## Why the guess had to go

The positional rule was "a bare word fills the first argument the published order
still has empty". The published order is the *declaration* order, and for
`send_message` that is `message name` because SPEC writes
`--message <message> --name <session>`. So `send_message reader hello` sent the
word `hello` to a session named `hello`, and the lines that meant it the other way
round -- `send_message reader "read @file(x)"` -- went to a session named after the
message. `ask_session.py` already records the same trap from the other side: "with
the positional form of `send_message`, the session name read as the question".
A one-shot line is read by nobody, so the mistake was silent: `2>&1` swallowed the
refusal and the test that should have failed passed.

## What a bare word gets now

    send_message takes its arguments by name, and "reader" names none;
    it reads send_message --message <text> --name <name>

Exit 2, one line, same shape as the other refusals. `--name value` and
`--name=value` are unchanged.

## What moved with it

- `bind()` and `usage()`: no positional branch left; `takes:` spells flags, so the
  shape the catalog prints is the shape a line has to have.
- `scripts/*.sh`: every invocation, by command and by tool command (`new`, `check`,
  `bash`, `python`, `read_file`, `subscribe`). Six `send_message` lines in
  `append_only`, `draft`, `upload` and `validity` were written session-first against
  the guess and are named by intent here, which is also the first time they reach
  the session they name.
- `cmd_selftest.sh` #4: the extra-word refusal now carries the bare-word sentence.
- `ship/skills`: `heir/transcript.py`'s `send_message` argv, the `--range` examples,
  `self-control`'s dry-run `set_session` words, `subagent`'s docstring.
- `docs/notes`: the examples in `client-lib.md` and `server-shell-command.md`.

## Still owed

- The window's own input line (`crates/client`'s `parse_args`, `crates/tui`) still
  binds bare words by position. SPEC spells flags everywhere, so it is the same
  question; it was out of scope for the one-shot CLI.
- `scripts/disk_selftest.sh` points at `<bin>/gongga-cli.exe`, which no build makes
  any more (`crates/cli` is gone). Red before this change, red after it.
