# `target` names the only machine, and the card says which one it is

User's instruction, 2026-11-02: implement the `target` SPEC declares on `remote_bash`,
and put the remote into the prompt's dynamic tail "so the agent knows its workspace is
on the remote".

## Why `target` was missing mattered

SPEC spells the tool `remote_bash{command, name, target = session's remote}`. The
implementation had no `target`, and `argument_error`'s rule -- decided earlier and left
alone here -- is that a *stray* key is passed through rather than failing the call. Put
together: a model following the SPEC wrote `target: "other-peer"`, the key was dropped,
and the command ran on the session's own peer with nothing said. That is the one failure
the remote work exists to prevent (a session that asked for a machine must never quietly
get another), reached through an argument nobody refused.

The fix is declaration, not strictness: `target` is in `remote_bash`'s schema, and
`BashTool::run` refuses any value but the session's peer, naming `set_session --remote`
as the way to move. The alternative -- rejecting every undeclared key -- was written and
then reverted: it reverses a recorded decision (a call that would have run is a call the
operator wanted) to fix a problem that declaring one argument fixes exactly.

## Why one machine, not many

A second peer would have no directory to be in: the session's cwd is the peer's path,
`@file:` in that peer's output would be read through the first peer's connection, and
`Owner` (which machine the directory belongs to) would stop meaning anything. SPEC
defines `target`'s cwd only for "session's remote is target", so the conservative branch
is also the literal one. Two peers are two sessions, and `set_session --remote` is the
one command that moves a session between them.

Supporting several would mean `Machine` keyed by name, a reader per peer, and
`Prompt.cwd`/`Prompt.remote` becoming a pair per machine. Not done, and the reason is
recorded here rather than in code: nothing in the SPEC asks for it.

## The card

The tail said the directory and never the machine, so a model holding
`/home/build/project` had no way to know it was the peer's -- it reached for `bash` and
found out by failing. A second template, `dynamic_tail_remote`, states the peer beside
the directory and says which tool is which:

    Session `x`, harness gongga at `...`, ..., in `/home/build/project` -- a directory
    on remote `build`, not on the machine this server runs on. `remote_bash` is that
    machine's shell; `bash` is this one's.

Two templates, not one with a clause: `substitute` has no conditionals, and a session on
this machine must keep the bytes it has already sent (its prefix stays warm). The name
comes from `Shells::remote_name` -- the same field `target` is answered with, so the
card cannot describe a machine the shells are not on -- and the prompt row records it
(`Prompt.remote`), because the template *choice* is a fact that has to be compared to
notice a move: `--remote none` in the directory the session left changes nothing else.

## What else moved

- `Shells` keeps the peer's name (`Peer { name, set }`) instead of a nameless `ShellSet`;
  `SessionManager::peer` resolves a name once, so name and endpoint cannot disagree.
- `remote_bash` declares `background`/`detached` no longer: they are refused, and a flag
  the model cannot use is not one to offer. Both refusals stay at run time, where they
  can say why.
- `docs/notes/server-remote.md`, `server-prompts.md`, `server-store.md`,
  `server-tools-mod.md` and the `remote` skill say all of it.

## How it was verified

- `cargo build --workspace` clean.
- `scripts/remote_selftest.sh` (real server, real peer, stand-in model): PASS.
- The card, by hand: a session moved onto the peer, its next request's first input item
  names the peer; moved home again with the same directory, and the new row says it does
  not.
- `remote_bash` with `target` set to its own peer runs; set to another configured name
  it refuses, and says `set_session --remote`.
